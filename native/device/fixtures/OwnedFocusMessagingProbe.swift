// Zero-input diagnosis of AX messaging on a newly registered owned TextEdit.
import AppKit
import ApplicationServices
import CryptoKit
import Darwin
import Foundation
import os

private struct Registration: Decodable, Sendable {
    let pid: UInt32, start_seconds: UInt64, start_micros: UInt64
    let executable_path: String, document_path: String, document_sha256: String
    let document_dev: UInt64, document_ino: UInt64, directory_dev: UInt64, directory_ino: UInt64

    func current() -> Bool {
        var process = proc_bsdinfo()
        guard pid > 0, pid <= UInt32(Int32.max),
              proc_pidinfo(Int32(pid), PROC_PIDTBSDINFO, 0, &process,
                Int32(MemoryLayout<proc_bsdinfo>.size)) == MemoryLayout<proc_bsdinfo>.size,
              process.pbi_start_tvsec == start_seconds, process.pbi_start_tvusec == start_micros,
              executable_path == "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
              NSRunningApplication(processIdentifier: Int32(pid))?.bundleIdentifier == "com.apple.TextEdit"
        else { return false }
        guard let canonicalPointer = realpath(document_path, nil),
              let temporaryPointer = realpath(NSTemporaryDirectory(), nil) else { return false }
        defer { free(canonicalPointer); free(temporaryPointer) }
        let canonical = String(cString: canonicalPointer)
        let temporary = String(cString: temporaryPointer)
        var path = [CChar](repeating: 0, count: 4096)
        guard proc_pidpath(Int32(pid), &path, UInt32(path.count)) > 0,
              path.withUnsafeBufferPointer({ String(cString: $0.baseAddress!) }) == executable_path,
              canonical == document_path,
              canonical.hasPrefix(temporary + "/"),
              URL(fileURLWithPath: canonical).lastPathComponent == "document.txt" else { return false }
        var file = stat(), directory = stat()
        guard lstat(canonical, &file) == 0,
              lstat((canonical as NSString).deletingLastPathComponent, &directory) == 0,
              file.st_uid == getuid(), directory.st_uid == getuid(), file.st_nlink == 1,
              (file.st_mode & S_IFMT) == S_IFREG, (directory.st_mode & S_IFMT) == S_IFDIR,
              file.st_mode & 0o777 == 0o600, directory.st_mode & 0o777 == 0o700,
              UInt64(file.st_dev) == document_dev, UInt64(file.st_ino) == document_ino,
              UInt64(directory.st_dev) == directory_dev, UInt64(directory.st_ino) == directory_ino,
              file.st_size >= 0, file.st_size <= 4096,
              let bytes = try? Data(contentsOf: URL(fileURLWithPath: canonical)),
              SHA256.hash(data: bytes).map({ String(format: "%02x", $0) }).joined() == document_sha256
        else { return false }
        return true
    }
}

private struct FocusRead: Encodable, Sendable {
    let thread: String, timeout_ms: UInt64, elapsed_ms: UInt64
    let setup_error: Int32, read_error: Int32?, element_type_valid: Bool
    let matches_owned_process: Bool, scope_current: Bool
}

private struct FocusDiagnostic: Encodable {
    let workspace_matches_owned_process: Bool
    let application_read_error: Int32, application_role_valid: Bool
    let cg_owned_window_present: Bool
    let observer_create_error: Int32, observer_add_error: Int32?
    let windows: [WindowFacts]
    let save_menu: OwnedSaveMenuInventory
    let reads: [FocusRead]
}

private struct WindowFacts: Encodable {
    let role_matches: Bool, subrole_matches: Bool, modal_false: Bool
    let document_matches: Bool, document_error: Int32, document_type_id: UInt64?
    let sheets_error: Int32, sheets_type_id: UInt64?, sheets_count: Int?
}

private func readWindows(_ registration: Registration, application: AXUIElement) -> [WindowFacts] {
    func read(_ element: AXUIElement, _ name: String) -> (CFTypeRef?, AXError) {
        guard AXUIElementSetMessagingTimeout(element, 0.04) == .success else { return (nil, .cannotComplete) }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        return (value, error)
    }
    let (raw, error) = read(application, kAXWindowsAttribute)
    guard error == .success, let raw, CFGetTypeID(raw) == CFArrayGetTypeID(),
          let windows = raw as? [AXUIElement], windows.count <= 32 else { return [] }
    return windows.map { window in
        let role = read(window, kAXRoleAttribute), subrole = read(window, kAXSubroleAttribute)
        let modal = read(window, kAXModalAttribute), document = read(window, kAXDocumentAttribute)
        let sheets = read(window, "AXSheets")
        var documentURL: URL?
        if let value = document.0 {
            if CFGetTypeID(value) == CFURLGetTypeID() {
                documentURL = unsafeDowncast(value, to: CFURL.self) as URL
            } else if let text = value as? String {
                documentURL = text.hasPrefix("/") ? URL(fileURLWithPath: text) : URL(string: text)
            }
        }
        var documentMatches = false
        if let url = documentURL, url.isFileURL, let path = realpath(url.path, nil) {
            documentMatches = String(cString: path) == registration.document_path
            free(path)
        }
        let sheetsCount: Int? = sheets.0.flatMap {
            CFGetTypeID($0) == CFArrayGetTypeID() ? CFArrayGetCount(unsafeDowncast($0, to: CFArray.self)) : nil
        }
        return WindowFacts(role_matches: role.1 == .success && role.0 as? String == kAXWindowRole,
            subrole_matches: subrole.1 == .success && subrole.0 as? String == kAXStandardWindowSubrole,
            modal_false: modal.1 == .success && modal.0.map { CFGetTypeID($0) == CFBooleanGetTypeID() }
                == true && modal.0 as? Bool == false,
            document_matches: document.1 == .success && documentMatches, document_error: document.1.rawValue,
            document_type_id: document.0.map { UInt64(CFGetTypeID($0)) },
            sheets_error: sheets.1.rawValue, sheets_type_id: sheets.0.map { UInt64(CFGetTypeID($0)) },
            sheets_count: sheetsCount)
    }
}

private func uptimeMS() -> UInt64 { DispatchTime.now().uptimeNanoseconds / 1_000_000 }

private func readFocus(_ registration: Registration, thread: String, timeoutMS: UInt64) -> FocusRead {
    let system = AXUIElementCreateSystemWide()
    let setup = AXUIElementSetMessagingTimeout(system, Float(timeoutMS) / 1000)
    let start = uptimeMS()
    var raw: CFTypeRef?
    let error = setup == .success
        ? AXUIElementCopyAttributeValue(system, kAXFocusedApplicationAttribute as CFString, &raw) : nil
    let elapsed = uptimeMS() - start
    let typed = raw.map { CFGetTypeID($0) == AXUIElementGetTypeID() } ?? false
    var owner: pid_t = 0
    let matches = error == .success && typed
        && AXUIElementGetPid(unsafeDowncast(raw!, to: AXUIElement.self), &owner) == .success
        && owner == Int32(registration.pid)
    return FocusRead(thread: thread, timeout_ms: timeoutMS, elapsed_ms: elapsed,
        setup_error: setup.rawValue, read_error: error?.rawValue, element_type_valid: typed,
        matches_owned_process: matches, scope_current: registration.current())
}

@main private struct OwnedFocusMessagingProbe {
    static func main() {
        guard CommandLine.arguments.dropFirst() == ["--probe"],
              let input = try? FileHandle.standardInput.readToEnd(), input.count <= 8192,
              let registration = try? JSONDecoder().decode(Registration.self, from: input),
              registration.current() else { exit(2) }
        guard AXIsProcessTrusted() else {
            FileHandle.standardOutput.write(Data("{\"accessibility_trusted\":false,\"scope_current\":true}\n".utf8))
            return
        }
        let results = OSAllocatedUnfairLock(initialState: [FocusRead]())
        func readWorker(_ name: String) -> Bool {
            let finished = DispatchSemaphore(value: 0)
            let worker = Thread {
                for timeout in [UInt64(40), UInt64(100)] {
                    let result = readFocus(registration, thread: name, timeoutMS: timeout)
                    results.withLock { $0.append(result) }
                }
                finished.signal()
            }
            worker.start()
            return finished.wait(timeout: .now() + 2) == .success
        }
        for timeout in [UInt64(40), UInt64(100)] {
            results.withLock { $0.append(readFocus(registration, thread: "main", timeoutMS: timeout)) }
        }
        guard readWorker("worker") else { exit(3) }
        let workspaceMatches = NSWorkspace.shared.frontmostApplication?.processIdentifier == Int32(registration.pid)
        for timeout in [UInt64(40), UInt64(100)] {
            results.withLock { $0.append(readFocus(registration, thread: "main_after_workspace", timeoutMS: timeout)) }
        }
        guard readWorker("worker_after_workspace"), registration.current() else { exit(3) }
        let application = AXUIElementCreateApplication(Int32(registration.pid))
        guard AXUIElementSetMessagingTimeout(application, 0.04) == .success else { exit(3) }
        var role: CFTypeRef?
        let appError = AXUIElementCopyAttributeValue(application, kAXRoleAttribute as CFString, &role)
        let appRoleValid = appError == .success && role as? String == kAXApplicationRole
        for timeout in [UInt64(40), UInt64(100)] {
            results.withLock { $0.append(readFocus(registration, thread: "main_after_app_read", timeoutMS: timeout)) }
        }
        guard readWorker("worker_after_app_read") else { exit(3) }
        let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
            as? [[String: Any]]
        let windowPresent = windows?.contains {
            ($0[kCGWindowOwnerPID as String] as? NSNumber)?.uint32Value == registration.pid
        } ?? false
        for timeout in [UInt64(40), UInt64(100)] {
            results.withLock { $0.append(readFocus(registration, thread: "main_after_window_list", timeoutMS: timeout)) }
        }
        guard readWorker("worker_after_window_list") else { exit(3) }
        var observer: AXObserver?
        let observerError = AXObserverCreate(Int32(registration.pid), { _, _, _, _ in }, &observer)
        var addError: AXError?
        if let observer {
            addError = AXObserverAddNotification(observer, application, kAXFocusedUIElementChangedNotification as CFString, nil)
            CFRunLoopAddSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(observer), .defaultMode)
            _ = CFRunLoopRunInMode(.defaultMode, 0.02, false)
        }
        for timeout in [UInt64(40), UInt64(100)] {
            results.withLock { $0.append(readFocus(registration, thread: "main_after_observer", timeoutMS: timeout)) }
        }
        guard readWorker("worker_after_observer"), registration.current() else { exit(3) }
        if let observer {
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(), AXObserverGetRunLoopSource(observer), .defaultMode)
            _ = AXObserverRemoveNotification(observer, application, kAXFocusedUIElementChangedNotification as CFString)
        }
        let diagnostic = FocusDiagnostic(workspace_matches_owned_process: workspaceMatches,
            application_read_error: appError.rawValue, application_role_valid: appRoleValid,
            cg_owned_window_present: windowPresent, observer_create_error: observerError.rawValue,
            observer_add_error: addError?.rawValue,
            windows: readWindows(registration, application: application),
            save_menu: readOwnedSaveMenu(application: application, pid: Int32(registration.pid)),
            reads: results.withLock { $0 })
        guard let data = try? JSONEncoder().encode(diagnostic) else { exit(4) }
        FileHandle.standardOutput.write(data + Data([10]))
    }
}
