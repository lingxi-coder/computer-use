import AppKit
import CoreFoundation
import Darwin
import Foundation

private enum FixtureFailure: Error {
    case invalidArguments
    case invalidProfile
    case invalidSnapshotPath
    case snapshotWriteFailed
}

private struct FixtureProfile {
    let taskID: String
    let seed: Int
    let variant: Variant
    let initialState: [String: Any]
    let snapshotPath: String
    let startInBackground: Bool

    struct Variant {
        let language: String
        let theme: String
        let layout: String
    }

    static func load(arguments: [String]) throws -> FixtureProfile {
        guard (arguments.count == 5 || (arguments.count == 6 && arguments[5] == "--background")),
              arguments[1] == "--profile",
              arguments[3] == "--snapshot",
              arguments[2].hasPrefix("/"),
              arguments[4].hasPrefix("/") else {
            throw FixtureFailure.invalidArguments
        }

        let rawData = try readBoundedProfile(at: arguments[2])
        guard let profile = try JSONSerialization.jsonObject(with: rawData) as? [String: Any],
              Set(profile.keys) == Set(["version", "task_id", "seed", "initial_state"]),
              integer(profile["version"]) == 1,
              let taskID = profile["task_id"] as? String,
              Self.taskIDs.contains(taskID),
              let seed = integer(profile["seed"]),
              (1...5).contains(seed),
              let initialState = profile["initial_state"] as? [String: Any],
              !containsForbiddenProfileKey(profile),
              let variantObject = initialState["variant"] as? [String: Any],
              Set(variantObject.keys) == Set(["language", "theme", "layout"]),
              let language = variantObject["language"] as? String,
              ["en-US", "zh-CN"].contains(language),
              let theme = variantObject["theme"] as? String,
              ["light", "dark"].contains(theme),
              let layout = variantObject["layout"] as? String,
              ["compact", "standard", "wide", "two_column"].contains(layout),
              validateInitialState(initialState, taskID: taskID, seed: seed) else {
            throw FixtureFailure.invalidProfile
        }

        try validateSnapshotPath(arguments[4])
        return FixtureProfile(
            taskID: taskID,
            seed: seed,
            variant: Variant(language: language, theme: theme, layout: layout),
            initialState: initialState,
            snapshotPath: arguments[4],
            startInBackground: arguments.count == 6
        )
    }

    private static let taskIDs: Set<String> = [
        "SET-01", "SET-02", "SET-03", "SET-04", "SET-05", "SET-06",
        "FORM-01", "FORM-02", "FORM-03", "FORM-04", "FORM-05", "FORM-06", "FORM-07",
        "LIST-01", "LIST-02", "LIST-03", "LIST-04", "MODAL-01", "MODAL-02", "MODAL-03",
    ]

    private static func integer(_ value: Any?) -> Int? {
        guard let number = value as? NSNumber,
              CFGetTypeID(number) != CFBooleanGetTypeID(),
              number.doubleValue.rounded(.towardZero) == number.doubleValue else {
            return nil
        }
        return number.intValue
    }

    private static func readBoundedProfile(at path: String) throws -> Data {
        guard path.hasPrefix("/") else {
            throw FixtureFailure.invalidProfile
        }
        let descriptor = Darwin.open(path, O_RDONLY | O_NOFOLLOW)
        guard descriptor >= 0 else { throw FixtureFailure.invalidProfile }
        defer { _ = Darwin.close(descriptor) }

        var fileStat = stat()
        guard Darwin.fstat(descriptor, &fileStat) == 0,
              (fileStat.st_mode & S_IFMT) == S_IFREG,
              fileStat.st_nlink == 1,
              fileStat.st_uid == getuid(),
              (fileStat.st_mode & 0o077) == 0,
              fileStat.st_size > 0,
              fileStat.st_size <= 128 * 1024 else {
            throw FixtureFailure.invalidProfile
        }

        let expectedSize = Int(fileStat.st_size)
        var data = Data()
        data.reserveCapacity(expectedSize)
        var buffer = [UInt8](repeating: 0, count: min(64 * 1024, expectedSize))
        while data.count < expectedSize {
            let requested = min(buffer.count, expectedSize - data.count)
            let readCount = buffer.withUnsafeMutableBufferPointer { bytes -> Int in
                guard let baseAddress = bytes.baseAddress else { return -1 }
                return Darwin.read(descriptor, UnsafeMutableRawPointer(baseAddress), requested)
            }
            if readCount < 0 && errno == EINTR { continue }
            guard readCount > 0 else { throw FixtureFailure.invalidProfile }
            data.append(contentsOf: buffer.prefix(readCount))
        }
        var afterRead = stat()
        guard Darwin.fstat(descriptor, &afterRead) == 0,
              afterRead.st_dev == fileStat.st_dev,
              afterRead.st_ino == fileStat.st_ino,
              afterRead.st_size == fileStat.st_size,
              data.count == expectedSize else {
            throw FixtureFailure.invalidProfile
        }
        return data
    }

    private static func validateSnapshotPath(_ path: String) throws {
        guard path.hasPrefix("/") else { throw FixtureFailure.invalidSnapshotPath }
        let parentPath = (path as NSString).deletingLastPathComponent
        guard let resolvedParent = realpath(parentPath, nil) else {
            throw FixtureFailure.invalidSnapshotPath
        }
        let resolvedParentPath = String(cString: resolvedParent)
        free(resolvedParent)
        guard resolvedParentPath == parentPath else { throw FixtureFailure.invalidSnapshotPath }

        var directoryStat = stat()
        guard Darwin.lstat(parentPath, &directoryStat) == 0,
              (directoryStat.st_mode & S_IFMT) == S_IFDIR,
              directoryStat.st_uid == getuid(),
              (directoryStat.st_mode & 0o077) == 0 else {
            throw FixtureFailure.invalidSnapshotPath
        }

        var existingStat = stat()
        if Darwin.lstat(path, &existingStat) == 0 {
            guard (existingStat.st_mode & S_IFMT) == S_IFREG,
                  existingStat.st_nlink == 1,
                  existingStat.st_uid == getuid() else {
                throw FixtureFailure.invalidSnapshotPath
            }
        } else if errno != ENOENT {
            throw FixtureFailure.invalidSnapshotPath
        }
    }

    private static func validateInitialState(_ state: [String: Any], taskID: String, seed: Int) -> Bool {
        guard let application = state["application"] as? [String: Any],
              Set(application.keys) == Set(["name", "bundle_id", "version"]),
              application["name"] as? String == "Interaction Fixture",
              application["bundle_id"] as? String == "com.lingxi.computer-user.mvp-fixture",
              application["version"] as? String == "1.0.0" else {
            return false
        }

        var expectedKeys: Set<String> = ["application", "variant"]
        switch taskID {
        case "SET-01", "SET-02", "SET-05", "SET-06",
             "FORM-01", "FORM-02", "FORM-03", "FORM-04", "FORM-05", "FORM-06", "FORM-07":
            expectedKeys.insert("controls")
            if taskID == "FORM-06" { expectedKeys.insert("validation_state") }
            if taskID == "SET-05" { expectedKeys.insert("persistence") }
        case "SET-03", "SET-04":
            expectedKeys.formUnion(["controls", "persistence", "seed"])
            guard integer(state["seed"]) == seed else { return false }
        case "LIST-01", "LIST-02", "LIST-03", "LIST-04":
            expectedKeys.insert("list")
        case "MODAL-01":
            expectedKeys.formUnion(["active_modal", "background_input"])
        case "MODAL-02":
            expectedKeys.formUnion(["active_modal", "controls"])
        case "MODAL-03":
            expectedKeys.formUnion(["active_modal", "background_controls"])
        default:
            return false
        }
        guard Set(state.keys) == expectedKeys else { return false }

        switch taskID {
        case "SET-01", "SET-02", "SET-03", "SET-04", "SET-05",
             "FORM-01", "FORM-02", "FORM-03", "FORM-04", "FORM-05", "FORM-06", "FORM-07":
            guard let controls = state["controls"] as? [[String: Any]], controls.count == 1,
                  controls[0]["label"] as? String != nil,
                  controls[0]["value"] != nil else { return false }
            if taskID == "SET-03" && state["persistence"] as? String != "manual_save_required" { return false }
            if taskID == "SET-04" && state["persistence"] as? String != "automatic" { return false }
            if taskID == "SET-05" && state["persistence"] as? String != "automatic" { return false }
            if taskID == "FORM-06" && state["validation_state"] as? String != "invalid" { return false }
            if taskID == "FORM-07" && controls[0]["initially_in_viewport"] as? Bool != false { return false }
        case "SET-06":
            guard let controls = state["controls"] as? [[String: Any]], controls.count == 2,
                  controls.allSatisfy({ $0["label"] as? String != nil && $0["group"] as? String != nil && $0["value"] as? Bool != nil }) else { return false }
        case "LIST-01":
            guard let list = state["list"] as? [String: Any],
                  let rows = list["rows"] as? [[String: Any]], rows.count == 1,
                  rows[0]["label"] as? String != nil else { return false }
        case "LIST-02":
            guard let list = state["list"] as? [String: Any],
                  let rows = list["same_named_rows"] as? [[String: Any]], rows.count == 2,
                  rows.allSatisfy({ $0["label"] as? String != nil && $0["owner"] as? String != nil && $0["status"] as? String != nil }) else { return false }
        case "LIST-03":
            guard let list = state["list"] as? [String: Any],
                  list["virtualized"] as? Bool == true,
                  integer(list["initially_rendered_rows"]) == 20,
                  list["target_accessible"] as? Bool == true else { return false }
        case "LIST-04":
            guard let list = state["list"] as? [String: Any],
                  list["sort"] as? String == "updated_descending",
                  let target = list["target"] as? [String: Any],
                  target["label"] as? String != nil,
                  target["status"] as? String != nil,
                  integer(target["updated_order"]) == 3,
                  integer(list["neighboring_rows"]) == 4 else { return false }
        case "MODAL-01", "MODAL-02", "MODAL-03":
            guard let modal = state["active_modal"] as? [String: Any],
                  modal["title"] as? String != nil else { return false }
            if taskID == "MODAL-01" && modal["kind"] as? String != "known_informational" { return false }
            if taskID == "MODAL-01" && state["background_input"] as? String != "must_remain_unchanged" { return false }
            if taskID == "MODAL-02" {
                guard let controls = state["controls"] as? [[String: Any]], controls.count == 1,
                      controls[0]["label"] as? String != nil,
                      controls[0]["value"] as? String != nil else { return false }
            }
            if taskID == "MODAL-03" {
                guard let controls = state["background_controls"] as? [[String: Any]], controls.count == 1,
                      controls[0]["label"] as? String != nil,
                      controls[0]["value"] as? Bool != nil else { return false }
            }
        default:
            return false
        }
        return true
    }

    private static func containsForbiddenProfileKey(_ value: Any) -> Bool {
        if let array = value as? [Any] { return array.contains(where: containsForbiddenProfileKey) }
        guard let object = value as? [String: Any] else { return false }
        for (key, child) in object {
            let normalized = key.lowercased()
            if ["objective", "allowed_scope", "evaluator", "evaluator_truth", "truth", "oracle", "navigation", "steps", "workflow", "selector", "coordinates"].contains(normalized)
                || normalized.hasPrefix("expected_") {
                return true
            }
            if containsForbiddenProfileKey(child) { return true }
        }
        return false
    }
}

@MainActor
private final class FixtureTextField: NSTextField {
    var onAccessibilityValueSet: (@MainActor (String) -> Void)?

    override func setAccessibilityValue(_ accessibilityValue: Any?) {
        super.setAccessibilityValue(accessibilityValue)
        guard let value = accessibilityValue as? String else { return }
        stringValue = value
        onAccessibilityValueSet?(value)
    }
}

@MainActor
private final class FixtureTextView: NSTextView {
    var onAccessibilityValueSet: (@MainActor (String) -> Void)?

    override func setAccessibilityValue(_ accessibilityValue: Any?) {
        super.setAccessibilityValue(accessibilityValue)
        guard let value = accessibilityValue as? String else { return }
        string = value
        onAccessibilityValueSet?(value)
    }
}

@MainActor
private final class FixtureCheckboxButton: NSButton {
    var onAccessibilityValueSet: (@MainActor (Bool) -> Void)?

    override func setAccessibilityValue(_ accessibilityValue: Any?) {
        super.setAccessibilityValue(accessibilityValue)
        guard let number = accessibilityValue as? NSNumber else { return }
        let integer = number.intValue
        guard (integer == 0 || integer == 1), number.doubleValue == Double(integer) else { return }
        let isOn = integer == 1
        state = isOn ? .on : .off
        onAccessibilityValueSet?(isOn)
    }
}

private enum SnapshotValue: Encodable, Sendable, Equatable {
    case string(String)
    case boolean(Bool)
    case integer(Int)

    var stringValue: String? {
        if case .string(let value) = self { return value }
        return nil
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .boolean(let value): try container.encode(value)
        case .integer(let value): try container.encode(value)
        }
    }
}

private struct SelectedRowSnapshot: Encodable, Sendable {
    let id: String
    let label: String
    let owner: String?
    let status: String?
    let position: Int

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(label, forKey: .label)
        try container.encode(owner, forKey: .owner)
        try container.encode(status, forKey: .status)
        try container.encode(position, forKey: .position)
    }

    private enum CodingKeys: String, CodingKey { case id, label, owner, status, position }
}

private struct ModalSnapshot: Encodable, Sendable {
    let open: Bool
    let choice: String?
    let controls: [String: SnapshotValue]

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(open, forKey: .open)
        try container.encode(choice, forKey: .choice)
        try container.encode(controls, forKey: .controls)
    }

    private enum CodingKeys: String, CodingKey { case open, choice, controls }
}

private struct SaveReceipt: Encodable, Sendable {
    let kind: String
    let revision: Int
}

private struct PersistenceSnapshot: Encodable, Sendable {
    let saved: Bool
    let kind: String
    let explicit_save_count: Int
    let autosave_count: Int
    let receipt: SaveReceipt?

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(saved, forKey: .saved)
        try container.encode(kind, forKey: .kind)
        try container.encode(explicit_save_count, forKey: .explicit_save_count)
        try container.encode(autosave_count, forKey: .autosave_count)
        try container.encode(receipt, forKey: .receipt)
    }

    private enum CodingKeys: String, CodingKey { case saved, kind, explicit_save_count, autosave_count, receipt }
}

private struct AuditSnapshot: Encodable, Sendable {
    let change_counts: [String: Int]
    let background_change_count: Int
    let other_row_change_count: Int
    let revision: Int
    let revisit_count: Int
    let row_rebind_count: Int
}

private struct FixtureSnapshot: Encodable, Sendable {
    let version: Int
    let task_id: String
    let seed: Int
    let values: [String: SnapshotValue]
    let selected_row: SelectedRowSnapshot?
    let modal: ModalSnapshot
    let persistence: PersistenceSnapshot
    let audit: AuditSnapshot

    func encode(to encoder: any Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(version, forKey: .version)
        try container.encode(task_id, forKey: .task_id)
        try container.encode(seed, forKey: .seed)
        try container.encode(values, forKey: .values)
        try container.encode(selected_row, forKey: .selected_row)
        try container.encode(modal, forKey: .modal)
        try container.encode(persistence, forKey: .persistence)
        try container.encode(audit, forKey: .audit)
    }

    private enum CodingKeys: String, CodingKey {
        case version, task_id, seed, values, selected_row, modal, persistence, audit
    }
}

@MainActor
private final class FixtureRow {
    let id: String
    let label: String
    let owner: String?
    let status: String?
    let updatedOrder: Int

    init(id: String, label: String, owner: String?, status: String?, updatedOrder: Int) {
        self.id = id
        self.label = label
        self.owner = owner
        self.status = status
        self.updatedOrder = updatedOrder
    }
}

@MainActor
private final class FlippedDocumentView: NSView {
    override var isFlipped: Bool { true }
}

@main
@MainActor
struct MvpFixtureMain {
    private static var retainedDelegate: FixtureApplicationDelegate?

    static func main() {
        do {
            let profile = try FixtureProfile.load(arguments: CommandLine.arguments)
            let application = NSApplication.shared
            application.setActivationPolicy(.regular)
            let delegate = FixtureApplicationDelegate(profile: profile)
            retainedDelegate = delegate
            application.delegate = delegate
            application.run()
        } catch {
            fputs("mvp-fixture: startup-rejected\n", stderr)
            exit(2)
        }
    }
}

@MainActor
private final class FixtureApplicationDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate,
    NSTextFieldDelegate, NSTextViewDelegate, NSTableViewDataSource, NSTableViewDelegate {
    private enum Page: Equatable { case settings, overview }

    private let profile: FixtureProfile
    private var values: [String: SnapshotValue]
    private var changeCounts: [String: Int]
    private var selectedRowID: String?
    private var rows: [FixtureRow] = []
    private var rowAccessibilityIdentifiers: [String: String] = [:]
    private var targetRowLabel: String?
    private var modalIsOpen: Bool
    private var modalChoice: String?
    private var modalControls: [String: SnapshotValue] = [:]
    private let persistenceKind: String
    private var persisted = false
    private var explicitSaveCount = 0
    private var autosaveCount = 0
    private var receipt: SaveReceipt?
    private var backgroundChangeCount = 0
    private var otherRowChangeCount = 0
    private var revision = 0
    private var revisitCount = 0
    private var rowRebindCount = 0
    private var hasLeftContent = false
    private var currentPage = Page.settings
    private var autosaveTask: Task<Void, Never>?

    private var window: NSWindow?
    private var bodyScrollView: NSScrollView?
    private var pageButtons: [NSButton] = []
    private var listTable: NSTableView?
    private var statusEditor: NSPopUpButton?
    private var persistenceReceiptLabel: NSTextField?
    private var regionValidationLabel: NSTextField?
    private var modalPanel: NSPanel?
    private var suppressSelectionEvents = false
    private var radioButtons: [String: [NSButton]] = [:]

    init(profile: FixtureProfile) {
        self.profile = profile
        self.values = Self.initialValues(for: profile)
        self.changeCounts = Self.initialChangeCounts(for: profile)
        self.modalIsOpen = profile.taskID.hasPrefix("MODAL-")
        self.persistenceKind = switch profile.taskID {
        case "SET-03": "manual"
        case "SET-04", "SET-05": "automatic"
        default: "none"
        }
        self.persisted = ["SET-03", "SET-04", "SET-05"].contains(profile.taskID)
        super.init()
        initializeModalControls()
        initializeRows()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        buildWindow()
        guard let window else { return }
        if profile.startInBackground {
            window.orderBack(nil)
        } else {
            window.makeKeyAndOrderFront(nil)
            NSApplication.shared.activate()
        }
        writeSnapshotOrTerminate()
        if modalIsOpen { presentInitialModal() }
        let ready = "{\"pid\":\(getpid()),\"window_number\":\(window.windowNumber)}"
        FileHandle.standardOutput.write(Data((ready + "\n").utf8))
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    func windowWillClose(_ notification: Notification) {
        autosaveTask?.cancel()
        NSApplication.shared.terminate(nil)
    }

    func controlTextDidChange(_ notification: Notification) {
        guard let field = notification.object as? NSTextField else { return }
        updateTextFieldValue(field.stringValue)
    }

    private func updateTextFieldValue(_ value: String) {
        switch profile.taskID {
        case "FORM-01": setValue("display_name", .string(value))
        case "FORM-06": updateRegionCode(value)
        case "FORM-07":
            guard let integer = Int(value) else {
                setValue("alert_threshold", .string(value))
                return
            }
            setValue("alert_threshold", .integer(integer))
        default: break
        }
    }

    func textDidChange(_ notification: Notification) {
        guard profile.taskID == "FORM-02", let textView = notification.object as? NSTextView else { return }
        updateNotesValue(textView.string)
    }

    private func updateNotesValue(_ value: String) {
        guard profile.taskID == "FORM-02" else { return }
        setValue("notes", .string(value))
    }

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        guard rows.indices.contains(row) else { return nil }
        rowRebindCount += 1
        let rowView = NSTableRowView(frame: .zero)
        rowView.setAccessibilityIdentifier(rowAccessibilityIdentifier(for: rows[row]))
        return rowView
    }

    private func rowAccessibilityIdentifier(for row: FixtureRow) -> String {
        if let identifier = rowAccessibilityIdentifiers[row.id] { return identifier }
        let identifier = UUID().uuidString.lowercased()
        rowAccessibilityIdentifiers[row.id] = identifier
        return identifier
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row rowIndex: Int) -> NSView? {
        guard rows.indices.contains(rowIndex), let column = tableColumn else { return nil }
        let identifier = NSUserInterfaceItemIdentifier("fixture-cell-\(column.identifier.rawValue)")
        let cell = (tableView.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView) ?? makeTableCell(identifier: identifier)
        cell.textField?.stringValue = cellValue(for: column.identifier.rawValue, row: rows[rowIndex])
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !suppressSelectionEvents, let table = notification.object as? NSTableView else { return }
        let newID = rows.indices.contains(table.selectedRow) ? rows[table.selectedRow].id : nil
        guard newID != selectedRowID else { return }
        selectedRowID = newID
        changeCounts["selected_row", default: 0] += 1
        revision += 1
        refreshStatusEditor()
        writeSnapshotOrTerminate()
    }

    @objc private func settingChanged(_ sender: NSButton) {
        guard let key = keyForControlTag(sender.tag) else { return }
        updateBooleanValue(key, sender.state == .on)
    }

    private func updateBooleanValue(_ key: String, _ value: Bool) {
        setValue(key, .boolean(value))
    }

    @objc private func selectionControlChanged(_ sender: NSPopUpButton) {
        guard let value = sender.selectedItem?.title else { return }
        switch profile.taskID {
        case "FORM-03": setValue("reminder_cadence", .string(value))
        case "LIST-04": updateSelectedListStatus(value)
        default: break
        }
    }

    @objc private func radioChanged(_ sender: NSButton) {
        let value = sender.title
        if let group = radioButtons["summary_grouping"] {
            for button in group { button.state = button === sender ? .on : .off }
        }
        setValue("summary_grouping", .string(value))
    }

    @objc private func saveExplicitly(_ sender: NSButton) {
        guard profile.taskID == "SET-03" else { return }
        explicitSaveCount += 1
        revision += 1
        persisted = true
        receipt = SaveReceipt(kind: "manual", revision: revision)
        updatePersistenceReceiptLabel()
        writeSnapshotOrTerminate()
    }

    @objc private func pageChanged(_ sender: NSButton) {
        let nextPage: Page = sender.tag == 1 ? .overview : .settings
        guard !modalIsOpen, pageButtons.contains(where: { $0 === sender }),
              nextPage != currentPage else { return }
        if currentPage == .settings && nextPage == .overview {
            hasLeftContent = true
        } else if currentPage == .overview && nextPage == .settings && hasLeftContent {
            revisitCount += 1
            hasLeftContent = false
        }
        for button in pageButtons { button.state = button === sender ? .on : .off }
        currentPage = nextPage
        revision += 1
        renderPage()
        writeSnapshotOrTerminate()
    }

    @objc private func closeInformationalSheet(_ sender: NSButton) {
        finishModal(choice: "dismiss")
    }

    @objc private func keepCurrentSelection(_ sender: NSButton) {
        finishModal(choice: "keep_current_selection")
    }

    @objc private func modalCategoryChanged(_ sender: NSPopUpButton) {
        guard let value = sender.selectedItem?.title,
              modalControls["reminder_category"] != .string(value) else { return }
        modalControls["reminder_category"] = .string(value)
        changeCounts["reminder_category", default: 0] += 1
        revision += 1
        writeSnapshotOrTerminate()
    }

    private var isChinese: Bool { profile.variant.language == "zh-CN" }

    private func localized(_ english: String, _ chinese: String) -> String {
        isChinese ? chinese : english
    }

    private static func initialValues(for profile: FixtureProfile) -> [String: SnapshotValue] {
        let state = profile.initialState
        let controls = state["controls"] as? [[String: Any]] ?? []
        func bool(_ index: Int) -> Bool { controls[index]["value"] as? Bool ?? false }
        func string(_ index: Int) -> String { controls[index]["value"] as? String ?? "" }
        func integerOrString(_ index: Int) -> SnapshotValue {
            if let value = controls[index]["value"] as? String { return .string(value) }
            if let number = controls[index]["value"] as? NSNumber { return .integer(number.intValue) }
            return .string("")
        }

        switch profile.taskID {
        case "SET-01": return ["email_alerts": .boolean(bool(0))]
        case "SET-02": return ["weekly_digest": .boolean(bool(0))]
        case "SET-03": return ["include_archived_items": .boolean(bool(0))]
        case "SET-04": return ["quiet_hours": .boolean(bool(0))]
        case "SET-05": return ["compact_rows": .boolean(bool(0))]
        case "SET-06": return ["system_show_alerts": .boolean(bool(0)), "workspace_show_alerts": .boolean(bool(1))]
        case "FORM-01": return ["display_name": .string(string(0))]
        case "FORM-02": return ["notes": .string(string(0))]
        case "FORM-03": return ["reminder_cadence": .string(string(0))]
        case "FORM-04": return ["summary_grouping": .string(string(0))]
        case "FORM-05": return ["notify_on_change": .boolean(bool(0))]
        case "FORM-06":
            return ["region_code": integerOrString(0), "validation_state": .string(state["validation_state"] as? String ?? "invalid")]
        case "FORM-07": return ["alert_threshold": integerOrString(0)]
        case "MODAL-01", "MODAL-02":
            return ["background_alerts": .boolean(false)]
        case "MODAL-03":
            let background = state["background_controls"] as? [[String: Any]] ?? []
            return ["background_alerts": .boolean(background.first?["value"] as? Bool ?? false)]
        default: return [:]
        }
    }

    private static func initialChangeCounts(for profile: FixtureProfile) -> [String: Int] {
        var keys: [String] = []
        switch profile.taskID {
        case "SET-01": keys = ["email_alerts"]
        case "SET-02": keys = ["weekly_digest"]
        case "SET-03": keys = ["include_archived_items"]
        case "SET-04": keys = ["quiet_hours"]
        case "SET-05": keys = ["compact_rows"]
        case "SET-06": keys = ["system_show_alerts", "workspace_show_alerts"]
        case "FORM-01": keys = ["display_name"]
        case "FORM-02": keys = ["notes"]
        case "FORM-03": keys = ["reminder_cadence"]
        case "FORM-04": keys = ["summary_grouping"]
        case "FORM-05": keys = ["notify_on_change"]
        case "FORM-06": keys = ["region_code", "validation_state"]
        case "FORM-07": keys = ["alert_threshold"]
        case "LIST-01", "LIST-02", "LIST-03": keys = ["selected_row"]
        case "LIST-04": keys = ["selected_row", "target_status"]
        case "MODAL-01": keys = ["modal_choice", "background_alerts"]
        case "MODAL-02": keys = ["reminder_category", "background_alerts"]
        case "MODAL-03": keys = ["modal_choice", "background_alerts"]
        default: break
        }
        return Dictionary(uniqueKeysWithValues: keys.map { ($0, 0) })
    }

    private func initializeModalControls() {
        guard profile.taskID == "MODAL-02",
              let controls = profile.initialState["controls"] as? [[String: Any]],
              let value = controls.first?["value"] as? String else { return }
        modalControls["reminder_category"] = .string(value)
    }

    private func initializeRows() {
        let list = profile.initialState["list"] as? [String: Any] ?? [:]
        switch profile.taskID {
        case "LIST-01":
            let item = (list["rows"] as? [[String: Any]])?.first ?? [:]
            let label = item["label"] as? String ?? ""
            let suffix = String(format: "%02d", profile.seed)
            rows = [FixtureRow(id: "quarterly_review_\(suffix)", label: label, owner: nil, status: nil, updatedOrder: 1)]
        case "LIST-02":
            let inputs = list["same_named_rows"] as? [[String: Any]] ?? []
            rows = inputs.enumerated().map { index, item in
                FixtureRow(
                    id: "weekly-report-row-\(index + 1)",
                    label: item["label"] as? String ?? "",
                    owner: item["owner"] as? String,
                    status: item["status"] as? String,
                    updatedOrder: inputs.count - index
                )
            }
        case "LIST-03":
            rows = (1...200).map { index in
                FixtureRow(
                    id: "archived_item_\(index)",
                    label: localized("Archived item \(index)", "已归档项目 \(index)"),
                    owner: nil,
                    status: nil,
                    updatedOrder: 201 - index
                )
            }
        case "LIST-04":
            let target = list["target"] as? [String: Any] ?? [:]
            let targetLabel = target["label"] as? String ?? ""
            let targetStatus = target["status"] as? String ?? ""
            let recordID = "record-03"
            targetRowLabel = targetLabel
            rows = [
                FixtureRow(id: "record-neighbor-a", label: localized("South team record", "南区团队记录"), owner: nil, status: localized("Open", "未处理"), updatedOrder: 5),
                FixtureRow(id: "record-neighbor-b", label: localized("West team record", "西区团队记录"), owner: nil, status: localized("Reviewed", "已审核"), updatedOrder: 4),
                FixtureRow(id: recordID, label: targetLabel, owner: nil, status: targetStatus, updatedOrder: 3),
                FixtureRow(id: "record-neighbor-c", label: localized("East team record", "东区团队记录"), owner: nil, status: localized("Open", "未处理"), updatedOrder: 2),
                FixtureRow(id: "record-neighbor-d", label: localized("Central team record", "中区团队记录"), owner: nil, status: localized("Reviewed", "已审核"), updatedOrder: 1),
            ]
            sortRows()
        default: break
        }
    }

    private func buildWindow() {
        let size: NSSize = switch profile.variant.layout {
        case "compact": NSSize(width: 760, height: 620)
        case "wide": NSSize(width: 1240, height: 760)
        case "two_column": NSSize(width: 1160, height: 740)
        default: NSSize(width: 940, height: 700)
        }
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Interaction Fixture"
        window.minSize = NSSize(width: 680, height: 500)
        window.delegate = self
        window.appearance = NSAppearance(named: profile.variant.theme == "dark" ? .darkAqua : .aqua)

        let root = NSView()
        root.wantsLayer = true
        root.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        window.contentView = root

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.drawsBackground = false
        scroll.borderType = .noBorder
        scroll.setAccessibilityLabel(localized("Preferences content", "偏好设置内容"))
        bodyScrollView = scroll

        root.addSubview(scroll)
        scroll.translatesAutoresizingMaskIntoConstraints = false
        let inset = contentInset
        var constraints = [
            scroll.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: inset),
            scroll.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -inset),
            scroll.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -inset),
        ]
        let settingsButton = NSButton(radioButtonWithTitle: localized("Settings", "设置"), target: self, action: #selector(pageChanged(_:)))
        let overviewButton = NSButton(radioButtonWithTitle: localized("Overview", "概览"), target: self, action: #selector(pageChanged(_:)))
        settingsButton.tag = 0
        overviewButton.tag = 1
        settingsButton.state = .on
        overviewButton.state = .off
        settingsButton.setAccessibilityLabel(localized("Settings", "设置"))
        overviewButton.setAccessibilityLabel(localized("Overview", "概览"))
        pageButtons = [settingsButton, overviewButton]
        updatePageNavigationEnabled()
        let navigation = horizontalStack(pageButtons, spacing: contentSpacing * 2)
        root.addSubview(navigation)
        navigation.translatesAutoresizingMaskIntoConstraints = false
        constraints.append(contentsOf: [
            navigation.topAnchor.constraint(equalTo: root.topAnchor, constant: inset),
            navigation.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: inset),
            navigation.trailingAnchor.constraint(lessThanOrEqualTo: root.trailingAnchor, constant: -inset),
            scroll.topAnchor.constraint(equalTo: navigation.bottomAnchor, constant: contentSpacing),
        ])
        NSLayoutConstraint.activate(constraints)
        self.window = window
        root.layoutSubtreeIfNeeded()
        renderPage()
        window.center()
    }

    private var contentInset: CGFloat {
        switch profile.variant.layout {
        case "compact": 12
        case "wide": 28
        case "two_column": 24
        default: 18
        }
    }

    private var contentSpacing: CGFloat {
        switch profile.variant.layout {
        case "compact": 7
        case "wide": 16
        case "two_column": 16
        default: 11
        }
    }

    private var controlWidth: CGFloat {
        switch profile.variant.layout {
        case "compact": 360
        case "wide": 700
        case "two_column": 610
        default: 500
        }
    }

    private func renderPage() {
        guard let bodyScrollView else { return }
        listTable = nil
        statusEditor = nil
        persistenceReceiptLabel = nil
        regionValidationLabel = nil
        let contents = currentPage == .settings ? buildSettingsContent() : buildOverviewContent()
        let width = max(400, bodyScrollView.contentSize.width)
        let inset = contentInset
        contents.setFrameSize(NSSize(width: width - inset * 2, height: 2_000))
        contents.layoutSubtreeIfNeeded()
        let contentHeight = max(1, contents.fittingSize.height)
        contents.setFrameSize(NSSize(width: width - inset * 2, height: contentHeight))

        let viewportHeight = max(400, bodyScrollView.contentSize.height)
        let documentHeight = max(viewportHeight, contentHeight + inset * 2)
        let document = FlippedDocumentView(frame: NSRect(x: 0, y: 0, width: width, height: documentHeight))
        document.autoresizingMask = [.width]
        document.appearance = window?.appearance
        document.wantsLayer = true
        document.layer?.backgroundColor = NSColor.windowBackgroundColor.cgColor
        document.addSubview(contents)
        contents.setFrameOrigin(NSPoint(x: inset, y: inset))
        contents.autoresizingMask = [.width]
        bodyScrollView.documentView = document
        bodyScrollView.contentView.scroll(to: .zero)
        bodyScrollView.reflectScrolledClipView(bodyScrollView.contentView)
        document.layoutSubtreeIfNeeded()
        if let listTable { restoreListSelection(in: listTable, reload: true) }
        updatePersistenceReceiptLabel()
    }

    private func updatePageNavigationEnabled() {
        for button in pageButtons { button.isEnabled = !modalIsOpen }
    }

    private func buildSettingsContent() -> NSView {
        var sections: [NSView] = [makeHeading(localized("Preferences", "偏好设置"))]
        switch profile.taskID {
        case "SET-01":
            sections.append(makeGroup(localized("Notifications", "通知"), [
                makeSwitch(localized("Email alerts", "邮件提醒"), key: "email_alerts"),
            ]))
        case "SET-02":
            sections.append(makeGroup(localized("Notifications", "通知"), [
                makeSwitch(localized("Weekly digest", "每周摘要"), key: "weekly_digest"),
            ]))
        case "SET-03":
            let control = makeSwitch(localized("Include archived items", "包含已归档项目"), key: "include_archived_items")
            let saveButton = makeButton(localized("Save", "保存"), action: #selector(saveExplicitly(_:)))
            let receiptLabel = makeLabel(localized("No saved change yet", "尚无已保存更改"), size: 13)
            receiptLabel.setAccessibilityLabel("fixture-persistence-receipt")
            persistenceReceiptLabel = receiptLabel
            sections.append(makeGroup(localized("Search", "搜索"), [
                control,
                horizontalStack([saveButton, receiptLabel], spacing: contentSpacing),
            ]))
        case "SET-04":
            let control = makeSwitch(localized("Quiet hours", "免打扰时段"), key: "quiet_hours")
            let receiptLabel = makeLabel(localized("All changes saved", "所有更改均已保存"), size: 13)
            receiptLabel.setAccessibilityLabel("fixture-persistence-receipt")
            persistenceReceiptLabel = receiptLabel
            sections.append(makeGroup(localized("Notifications", "通知"), [control, receiptLabel]))
        case "SET-05":
            sections.append(makeGroup(localized("Display", "显示"), [
                makeSwitch(localized("Compact rows", "紧凑行"), key: "compact_rows"),
            ]))
        case "SET-06":
            let firstGroup = makeGroup(localized("System notifications", "系统通知"),
                [makeSwitch(localized("Show alerts", "显示提醒"), key: "system_show_alerts")])
            let secondGroup = makeGroup(localized("Workspace notifications", "工作区通知"),
                [makeSwitch(localized("Show alerts", "显示提醒"), key: "workspace_show_alerts")])
            sections.append(arrangeColumns([firstGroup, secondGroup]))
        case "FORM-01":
            sections.append(makeGroup(localized("Profile", "个人资料"), [
                makeTextFieldRow(label: localized("Display name", "显示名称"), value: valueString("display_name"), singleLine: true),
            ]))
        case "FORM-02":
            sections.append(makeGroup(localized("Profile", "个人资料"), [makeMultilineNotes()]))
        case "FORM-03":
            sections.append(makeGroup(localized("Preferences", "偏好设置"), [makeReminderCadence()]))
        case "FORM-04":
            sections.append(makeGroup(localized("Preferences", "偏好设置"), [makeSummaryGrouping()]))
        case "FORM-05":
            sections.append(makeGroup(localized("Preferences", "偏好设置"), [
                makeSwitch(localized("Notify on change", "更改时通知"), key: "notify_on_change"),
            ]))
        case "FORM-06":
            let control = makeTextFieldRow(label: localized("Region code", "地区代码"), value: valueString("region_code"), singleLine: true)
            let validation = makeLabel(validationDescription, size: 13)
            validation.setAccessibilityLabel("fixture-region-code-validation")
            regionValidationLabel = validation
            sections.append(makeGroup(localized("Account", "账户"), [control, validation]))
        case "FORM-07":
            sections.append(makeGroup(localized("Advanced preferences", "高级偏好设置"), makeLongAdvancedSettings()))
        case "LIST-01", "LIST-02", "LIST-03", "LIST-04":
            sections.append(makeListSection())
        case "MODAL-01", "MODAL-02", "MODAL-03":
            sections.append(makeModalBackgroundSection())
        default:
            break
        }
        return verticalStack(sections, spacing: contentSpacing)
    }

    private func buildOverviewContent() -> NSView {
        let title = makeHeading(localized("Account overview", "账户概览"))
        let detail = makeLabel(localized("Review your settings and return whenever you are ready.", "查看设置；准备好后可返回。"), size: 14)
        return verticalStack([title, detail], spacing: contentSpacing)
    }

    private func makeLongAdvancedSettings() -> [NSView] {
        let entries: [(String, String, String, String)] = [
            ("General", "General account preferences", "常规", "账户常规偏好设置"),
            ("Notifications", "Choose which updates are visible", "通知", "选择要显示的更新"),
            ("Privacy", "Review how information is presented", "隐私", "查看信息呈现方式"),
            ("Search", "Adjust search and discovery behavior", "搜索", "调整搜索和发现行为"),
            ("Accessibility", "Review display and interaction options", "辅助功能", "查看显示和交互选项"),
            ("Connected apps", "Manage connected application details", "已连接应用", "管理已连接应用信息"),
            ("Data", "Review storage and synchronization details", "数据", "查看存储和同步信息"),
            ("Advanced", "Additional account preferences", "高级", "其他账户偏好设置"),
            ("Appearance", "Review color and density preferences", "外观", "查看颜色和密度偏好设置"),
            ("Language", "Review language preferences", "语言", "查看语言偏好设置"),
            ("Shortcuts", "Review keyboard shortcut preferences", "快捷键", "查看键盘快捷键偏好设置"),
            ("Storage", "Review local storage preferences", "存储", "查看本地存储偏好设置"),
        ]
        var items: [NSView] = []
        for (englishTitle, englishDetail, chineseTitle, chineseDetail) in entries {
            let card = verticalStack([
                makeLabel(localized(englishTitle, chineseTitle), size: 15, weight: .semibold),
                makeLabel(localized(englishDetail, chineseDetail), size: 12),
            ], spacing: 4)
            card.setContentHuggingPriority(.defaultLow, for: .horizontal)
            card.heightAnchor.constraint(greaterThanOrEqualToConstant: 66).isActive = true
            items.append(card)
        }
        items.append(makeTextFieldRow(label: localized("Alert threshold", "提醒阈值"), value: valueString("alert_threshold"), singleLine: true, isNumber: true))
        return items
    }

    private func makeSwitch(_ title: String, key: String) -> NSButton {
        let button = FixtureCheckboxButton(checkboxWithTitle: title, target: self, action: #selector(settingChanged(_:)))
        button.state = valueBoolean(key) ? .on : .off
        button.tag = controlTag(for: key)
        button.onAccessibilityValueSet = { [weak self] isOn in
            self?.updateBooleanValue(key, isOn)
        }
        button.setAccessibilityLabel(title)
        button.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return button
    }

    private func makeTextFieldRow(label: String, value: String, singleLine: Bool, isNumber: Bool = false) -> NSView {
        let name = makeLabel(label, size: 14)
        name.widthAnchor.constraint(equalToConstant: 170).isActive = true
        let field = FixtureTextField(string: value)
        field.delegate = self
        field.onAccessibilityValueSet = { [weak self] value in
            self?.updateTextFieldValue(value)
        }
        field.isEditable = true
        field.isSelectable = true
        field.isBordered = true
        field.isBezeled = true
        field.focusRingType = .default
        field.setAccessibilityLabel(label)
        field.placeholderString = isNumber ? localized("Enter a number", "输入数字") : nil
        field.widthAnchor.constraint(equalToConstant: controlWidth).isActive = true
        field.isContinuous = true
        field.setContentHuggingPriority(.defaultLow, for: .horizontal)
        _ = singleLine
        return horizontalStack([name, field], spacing: contentSpacing)
    }

    private func makeMultilineNotes() -> NSView {
        let label = makeLabel(localized("Notes", "备注"), size: 14)
        label.setAccessibilityLabel(localized("Notes", "备注"))
        let textView = FixtureTextView(frame: NSRect(x: 0, y: 0, width: controlWidth, height: 120))
        textView.delegate = self
        textView.string = valueString("notes")
        textView.onAccessibilityValueSet = { [weak self] value in
            self?.updateNotesValue(value)
        }
        textView.isRichText = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.setAccessibilityLabel(localized("Notes", "备注"))
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: controlWidth, height: 132))
        scroll.documentView = textView
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.setAccessibilityLabel(localized("Notes", "备注"))
        let row = verticalStack([label, scroll], spacing: contentSpacing / 2)
        return row
    }

    private func makeReminderCadence() -> NSView {
        let label = localized("Reminder cadence", "提醒频率")
        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        popup.addItems(withTitles: [localized("Daily", "每天"), localized("Weekly", "每周"), localized("Monthly", "每月")])
        popup.selectItem(withTitle: valueString("reminder_cadence"))
        popup.target = self
        popup.action = #selector(selectionControlChanged(_:))
        popup.setAccessibilityLabel(label)
        popup.widthAnchor.constraint(equalToConstant: min(controlWidth, 360)).isActive = true
        return makeControlRow(label: label, control: popup)
    }

    private func makeSummaryGrouping() -> NSView {
        let label = localized("Summary grouping", "摘要分组")
        let groupValues = [localized("By day", "按天"), localized("By week", "按周"), localized("By month", "按月")]
        var buttons: [NSButton] = []
        for (index, title) in groupValues.enumerated() {
            let button = NSButton(radioButtonWithTitle: title, target: self, action: #selector(radioChanged(_:)))
            button.identifier = NSUserInterfaceItemIdentifier("summary_grouping_\(index)")
            button.setAccessibilityLabel(title)
            button.state = title == valueString("summary_grouping") ? .on : .off
            buttons.append(button)
        }
        radioButtons["summary_grouping"] = buttons
        let stack = verticalStack(buttons, spacing: max(2, contentSpacing / 2))
        stack.setAccessibilityElement(true)
        stack.setAccessibilityRole(.radioGroup)
        stack.setAccessibilityLabel(label)
        return makeControlRow(label: label, control: stack)
    }

    private func makeControlRow(label: String, control: NSView) -> NSView {
        let labelView = makeLabel(label, size: 14)
        labelView.widthAnchor.constraint(equalToConstant: 170).isActive = true
        return horizontalStack([labelView, control], spacing: contentSpacing)
    }

    private func makeListSection() -> NSView {
        let title: String
        let accessibilityLabel: String
        switch profile.taskID {
        case "LIST-01": title = localized("Saved views", "已保存视图"); accessibilityLabel = title
        case "LIST-02": title = localized("Projects", "项目"); accessibilityLabel = title
        case "LIST-03": title = localized("Archive", "归档"); accessibilityLabel = title
        default: title = localized("Records", "记录"); accessibilityLabel = title
        }
        let table = NSTableView()
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = true
        table.rowHeight = profile.taskID == "LIST-03" ? 22 : 30
        table.selectionHighlightStyle = .regular
        table.dataSource = self
        table.delegate = self
        configureColumns(for: table)
        listTable = table

        let tableScroll = NSScrollView(frame: .zero)
        tableScroll.documentView = table
        tableScroll.hasVerticalScroller = true
        tableScroll.hasHorizontalScroller = false
        tableScroll.borderType = .bezelBorder
        tableScroll.setAccessibilityLabel(accessibilityLabel)
        tableScroll.heightAnchor.constraint(equalToConstant: profile.taskID == "LIST-03" ? 500 : 300).isActive = true
        let minimumTableWidth: CGFloat = switch profile.taskID {
        case "LIST-02": 620
        case "LIST-04": 650
        default: 550
        }
        tableScroll.widthAnchor.constraint(equalToConstant: max(controlWidth + 170, minimumTableWidth)).isActive = true

        var items: [NSView] = [makeHeading(title), tableScroll]
        if profile.taskID == "LIST-04" {
            let statusLabel = localized("Status", "状态")
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.addItems(withTitles: [localized("Open", "未处理"), localized("Reviewed", "已审核")])
            popup.selectItem(withTitle: valueString("target_status"))
            popup.target = self
            popup.action = #selector(selectionControlChanged(_:))
            popup.setAccessibilityLabel(statusLabel)
            popup.isEnabled = false
            statusEditor = popup
            items.append(makeControlRow(label: statusLabel, control: popup))
        }
        return verticalStack(items, spacing: contentSpacing)
    }

    private func configureColumns(for table: NSTableView) {
        let definitions: [(String, String, CGFloat)] = switch profile.taskID {
        case "LIST-02": [("label", localized("Name", "名称"), 300), ("owner", localized("Owner", "负责人"), 130), ("status", localized("Status", "状态"), 150)]
        case "LIST-04": [("label", localized("Record", "记录"), 330), ("status", localized("Status", "状态"), 160), ("updated", localized("Updated", "更新时间"), 140)]
        case "LIST-03": [("label", localized("Archive item", "归档项目"), 480)]
        default: [("label", localized("Entry", "条目"), 500)]
        }
        for (identifier, title, width) in definitions {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.title = title
            column.width = width
            column.minWidth = min(80, width)
            table.addTableColumn(column)
        }
    }

    private func makeModalBackgroundSection() -> NSView {
        if profile.taskID == "MODAL-03" {
            let state = profile.initialState["background_controls"] as? [[String: Any]] ?? []
            let label = state.first?["label"] as? String ?? localized("Background alerts", "背景提醒")
            let control = NSButton(checkboxWithTitle: label, target: self, action: #selector(backgroundControlChanged(_:)))
            control.state = state.first?["value"] as? Bool == true ? .on : .off
            control.tag = controlTag(for: "background_alerts")
            control.setAccessibilityLabel(label)
            return makeGroup(localized("Preferences", "偏好设置"), [control])
        }
        let label = makeLabel(localized("Page alerts", "页面提醒"), size: 14)
        let control = NSButton(checkboxWithTitle: localized("Email alerts", "邮件提醒"), target: self, action: #selector(backgroundControlChanged(_:)))
        control.state = .off
        control.tag = controlTag(for: "background_alerts")
        return makeGroup(localized("Preferences", "偏好设置"), [label, control])
    }

    private func makeGroup(_ title: String, _ views: [NSView]) -> NSView {
        let box = NSBox()
        box.title = title
        box.boxType = .primary
        box.setAccessibilityLabel(title)
        box.contentViewMargins = NSSize(width: contentSpacing, height: contentSpacing)
        let stack = verticalStack(views, spacing: contentSpacing / 2)
        box.contentView = stack
        box.setContentHuggingPriority(.defaultLow, for: .horizontal)
        return box
    }

    private func arrangeColumns(_ views: [NSView]) -> NSView {
        guard profile.variant.layout == "two_column", views.count > 1 else {
            return verticalStack(views, spacing: contentSpacing)
        }
        let midpoint = (views.count + 1) / 2
        let left = verticalStack(Array(views[..<midpoint]), spacing: contentSpacing)
        let right = verticalStack(Array(views[midpoint...]), spacing: contentSpacing)
        let columns = horizontalStack([left, right], spacing: contentSpacing * 2)
        columns.distribution = .fillEqually
        return columns
    }

    private func verticalStack(_ views: [NSView], spacing: CGFloat) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.distribution = .fill
        stack.spacing = spacing
        return stack
    }

    private func horizontalStack(_ views: [NSView], spacing: CGFloat) -> NSStackView {
        let stack = NSStackView(views: views)
        stack.orientation = .horizontal
        stack.alignment = .centerY
        stack.distribution = .fill
        stack.spacing = spacing
        return stack
    }

    private func makeHeading(_ text: String) -> NSTextField {
        makeLabel(text, size: 23, weight: .semibold)
    }

    private func makeLabel(_ text: String, size: CGFloat, weight: NSFont.Weight = .regular) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = NSFont.systemFont(ofSize: size, weight: weight)
        label.textColor = .labelColor
        label.lineBreakMode = .byWordWrapping
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return label
    }

    private func makeButton(_ title: String, action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.bezelStyle = .rounded
        button.setAccessibilityLabel(title)
        return button
    }

    private func makeTableCell(identifier: NSUserInterfaceItemIdentifier) -> NSTableCellView {
        let cell = NSTableCellView(frame: .zero)
        cell.identifier = identifier
        let textField = NSTextField(labelWithString: "")
        textField.lineBreakMode = .byTruncatingTail
        textField.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        textField.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(textField)
        cell.textField = textField
        NSLayoutConstraint.activate([
            textField.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 7),
            textField.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -7),
            textField.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    private func cellValue(for column: String, row: FixtureRow) -> String {
        switch column {
        case "owner": return row.owner ?? ""
        case "status": return row.status ?? ""
        case "updated": return String(row.updatedOrder)
        default: return row.label
        }
    }

    private func refreshStatusEditor() {
        guard profile.taskID == "LIST-04", let statusEditor else { return }
        guard let selectedRowID,
              let row = rows.first(where: { $0.id == selectedRowID }) else {
            statusEditor.isEnabled = false
            return
        }
        statusEditor.isEnabled = true
        if let status = row.status { statusEditor.selectItem(withTitle: status) }
    }

    private func updateSelectedListStatus(_ status: String) {
        guard let selectedRowID,
              let index = rows.firstIndex(where: { $0.id == selectedRowID }),
              rows[index].status != status else { return }

        let changedRow = rows[index]
        let changedTarget = changedRow.label == targetRowLabel
        rows = rows.map { row in
            FixtureRow(
                id: row.id,
                label: row.label,
                owner: row.owner,
                status: row.id == selectedRowID ? status : row.status,
                updatedOrder: row.id == selectedRowID ? rows.count + 1 : row.updatedOrder
            )
        }
        if changedTarget {
            values["target_status"] = .string(status)
            changeCounts["target_status", default: 0] += 1
            revision += 1
            sortRows()
            reloadListPreservingSelection()
        } else {
            otherRowChangeCount += 1
            revision += 1
            sortRows()
            reloadListPreservingSelection()
        }
        refreshStatusEditor()
        writeSnapshotOrTerminate()
    }

    private func sortRows() {
        rows.sort { left, right in left.updatedOrder > right.updatedOrder }
    }

    private func reloadListPreservingSelection() {
        guard let listTable else { return }
        restoreListSelection(in: listTable, reload: true)
    }

    private func restoreListSelection(in table: NSTableView, reload: Bool) {
        let previousSuppression = suppressSelectionEvents
        suppressSelectionEvents = true
        defer { suppressSelectionEvents = previousSuppression }
        if reload { table.reloadData() }
        if let selectedRowID, let index = rows.firstIndex(where: { $0.id == selectedRowID }) {
            table.selectRowIndexes(IndexSet(integer: index), byExtendingSelection: false)
        }
    }

    private func keyForControlTag(_ tag: Int) -> String? {
        switch tag {
        case 1: return "email_alerts"
        case 2: return "weekly_digest"
        case 3: return "include_archived_items"
        case 4: return "quiet_hours"
        case 5: return "compact_rows"
        case 6: return "system_show_alerts"
        case 7: return "workspace_show_alerts"
        case 8: return "notify_on_change"
        case 9: return "background_alerts"
        default: return nil
        }
    }

    private func controlTag(for key: String) -> Int {
        switch key {
        case "email_alerts": 1
        case "weekly_digest": 2
        case "include_archived_items": 3
        case "quiet_hours": 4
        case "compact_rows": 5
        case "system_show_alerts": 6
        case "workspace_show_alerts": 7
        case "notify_on_change": 8
        case "background_alerts": 9
        default: 0
        }
    }

    private func valueBoolean(_ key: String) -> Bool {
        if case .boolean(let value) = values[key] { return value }
        return false
    }

    private func valueString(_ key: String) -> String {
        switch values[key] {
        case .string(let value): return value
        case .integer(let value): return String(value)
        case .boolean(let value): return value ? "true" : "false"
        case .none: return ""
        }
    }

    @objc private func backgroundControlChanged(_ sender: NSButton) {
        let nextValue = SnapshotValue.boolean(sender.state == .on)
        guard values["background_alerts"] != nextValue else { return }
        backgroundChangeCount += 1
        setValue("background_alerts", nextValue)
    }

    private func updateRegionCode(_ value: String) {
        setValue("region_code", .string(value))
        let nextState = value.range(of: #"^CA-[0-9]{4}$"#, options: .regularExpression) == nil ? "invalid" : "valid"
        setValue("validation_state", .string(nextState))
        regionValidationLabel?.stringValue = validationDescription
    }

    private var validationDescription: String {
        valueString("validation_state") == "valid"
            ? localized("Region code is valid", "地区代码有效")
            : localized("Enter a region code in the form CA-1234", "请输入 CA-1234 格式的地区代码")
    }

    private func setValue(_ key: String, _ value: SnapshotValue) {
        guard values[key] != value else { return }
        values[key] = value
        changeCounts[key, default: 0] += 1
        revision += 1
        if persistenceKind != "none" {
            persisted = false
            receipt = nil
            if persistenceKind == "automatic" { scheduleAutosave() }
            updatePersistenceReceiptLabel()
        }
        writeSnapshotOrTerminate()
    }

    private func scheduleAutosave() {
        autosaveTask?.cancel()
        autosaveTask = Task { @MainActor [weak self] in
            do {
                try await Task.sleep(for: .milliseconds(800))
            } catch {
                return
            }
            guard let self, !Task.isCancelled, self.persistenceKind == "automatic" else { return }
            self.persisted = true
            self.autosaveCount += 1
            self.revision += 1
            self.receipt = SaveReceipt(kind: "automatic", revision: self.revision)
            self.autosaveTask = nil
            self.updatePersistenceReceiptLabel()
            self.writeSnapshotOrTerminate()
        }
    }

    private func updatePersistenceReceiptLabel() {
        guard let persistenceReceiptLabel else { return }
        if profile.taskID == "SET-03" {
            if let receipt {
                persistenceReceiptLabel.stringValue = localized("Saved manually · revision \(receipt.revision)", "已手动保存 · 版本 \(receipt.revision)")
            } else {
                persistenceReceiptLabel.stringValue = persisted
                    ? localized("No saved change yet", "尚无已保存更改")
                    : localized("Unsaved changes", "有未保存的更改")
            }
        } else if profile.taskID == "SET-04" {
            if let receipt {
                persistenceReceiptLabel.stringValue = localized("Saved automatically · revision \(receipt.revision)", "已自动保存 · 版本 \(receipt.revision)")
            } else {
                persistenceReceiptLabel.stringValue = persisted
                    ? localized("All changes saved", "所有更改均已保存")
                    : localized("Saving changes…", "正在保存更改…")
            }
        }
    }

    private func finishModal(choice: String) {
        guard modalIsOpen else { return }
        modalIsOpen = false
        modalChoice = choice
        changeCounts["modal_choice", default: 0] += 1
        revision += 1
        if let panel = modalPanel, let window {
            window.endSheet(panel, returnCode: .OK)
            panel.orderOut(nil)
        }
        modalPanel = nil
        updatePageNavigationEnabled()
        writeSnapshotOrTerminate()
    }

    private func presentInitialModal() {
        guard let window else { return }
        let size: NSSize = switch profile.taskID {
        case "MODAL-02": NSSize(width: 520, height: 280)
        case "MODAL-03": NSSize(width: 500, height: 250)
        default: NSSize(width: 480, height: 240)
        }
        let panel = NSPanel(
            contentRect: NSRect(origin: .zero, size: size),
            styleMask: [.titled],
            backing: .buffered,
            defer: false
        )
        panel.isFloatingPanel = false
        panel.appearance = window.appearance
        let title: String
        let message: String
        switch profile.taskID {
        case "MODAL-01":
            title = localized("Session notice", "会话提示")
            message = localized("This notice blocks the page until you continue.", "此提示会阻挡页面，继续后才能返回页面。")
        case "MODAL-02":
            title = localized("Create reminder", "创建提醒")
            message = localized("Choose a reminder category.", "选择提醒类别。")
        default:
            title = localized("Confirm preferences", "确认偏好设置")
            message = localized("Review the current selection.", "查看当前选择。")
        }
        panel.title = title
        let panelStackViews: [NSView] = [makeLabel(title, size: 18), makeLabel(message, size: 14)]
        let contentViews: [NSView]
        if profile.taskID == "MODAL-02" {
            let label = localized("Reminder category", "提醒类别")
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.addItems(withTitles: [localized("Other", "其他"), localized("Personal", "个人"), localized("Work", "工作")])
            popup.selectItem(withTitle: modalControls["reminder_category"]?.stringValue ?? localized("Other", "其他"))
            popup.target = self
            popup.action = #selector(modalCategoryChanged(_:))
            popup.setAccessibilityLabel(label)
            popup.widthAnchor.constraint(equalToConstant: 300).isActive = true
            contentViews = panelStackViews + [makeControlRow(label: label, control: popup)]
        } else if profile.taskID == "MODAL-03" {
            contentViews = panelStackViews + [makeButton(localized("Keep current selection", "保留当前选择"), action: #selector(keepCurrentSelection(_:)))]
        } else {
            contentViews = panelStackViews + [makeButton(localized("Continue", "继续"), action: #selector(closeInformationalSheet(_:)))]
        }

        let content = verticalStack(contentViews, spacing: contentSpacing)
        let panelContent = NSView(frame: NSRect(origin: .zero, size: size))
        panel.contentView = panelContent
        panelContent.addSubview(content)
        content.translatesAutoresizingMaskIntoConstraints = false
        NSLayoutConstraint.activate([
            content.leadingAnchor.constraint(equalTo: panelContent.leadingAnchor, constant: contentInset),
            content.trailingAnchor.constraint(equalTo: panelContent.trailingAnchor, constant: -contentInset),
            content.topAnchor.constraint(equalTo: panelContent.topAnchor, constant: contentInset),
            content.bottomAnchor.constraint(lessThanOrEqualTo: panelContent.bottomAnchor, constant: -contentInset),
        ])
        modalPanel = panel
        window.beginSheet(panel, completionHandler: nil)
        writeSnapshotOrTerminate()
    }

    private var selectedRowSnapshot: SelectedRowSnapshot? {
        guard let selectedRowID,
              let index = rows.firstIndex(where: { $0.id == selectedRowID }) else { return nil }
        let row = rows[index]
        return SelectedRowSnapshot(id: row.id, label: row.label, owner: row.owner, status: row.status, position: index + 1)
    }

    private func snapshot() -> FixtureSnapshot {
        FixtureSnapshot(
            version: 1,
            task_id: profile.taskID,
            seed: profile.seed,
            values: values,
            selected_row: selectedRowSnapshot,
            modal: ModalSnapshot(open: modalIsOpen, choice: modalChoice, controls: modalControls),
            persistence: PersistenceSnapshot(
                saved: persisted,
                kind: persistenceKind,
                explicit_save_count: explicitSaveCount,
                autosave_count: autosaveCount,
                receipt: receipt
            ),
            audit: AuditSnapshot(
                change_counts: changeCounts,
                background_change_count: backgroundChangeCount,
                other_row_change_count: otherRowChangeCount,
                revision: revision,
                revisit_count: revisitCount,
                row_rebind_count: rowRebindCount
            )
        )
    }

    private func writeSnapshotOrTerminate() {
        do {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.sortedKeys]
            let data = try encoder.encode(snapshot())
            try atomicWrite(data, to: profile.snapshotPath)
        } catch {
            fputs("mvp-fixture: snapshot-write-failed\n", stderr)
            NSApplication.shared.terminate(nil)
        }
    }

    private func atomicWrite(_ data: Data, to path: String) throws {
        let parent = (path as NSString).deletingLastPathComponent
        guard path.hasPrefix("/"), let resolvedParent = realpath(parent, nil) else {
            throw FixtureFailure.invalidSnapshotPath
        }
        let resolvedParentPath = String(cString: resolvedParent)
        free(resolvedParent)
        guard resolvedParentPath == parent else { throw FixtureFailure.invalidSnapshotPath }

        var directoryStat = stat()
        guard Darwin.lstat(parent, &directoryStat) == 0,
              (directoryStat.st_mode & S_IFMT) == S_IFDIR,
              directoryStat.st_uid == getuid(),
              (directoryStat.st_mode & 0o077) == 0 else {
            throw FixtureFailure.invalidSnapshotPath
        }
        var currentStat = stat()
        if Darwin.lstat(path, &currentStat) == 0 {
            guard (currentStat.st_mode & S_IFMT) == S_IFREG,
                  currentStat.st_nlink == 1,
                  currentStat.st_uid == getuid() else {
                throw FixtureFailure.invalidSnapshotPath
            }
        } else if errno != ENOENT {
            throw FixtureFailure.invalidSnapshotPath
        }

        let temporaryPath = "\(parent)/.mvp-fixture-\(UUID().uuidString).tmp"
        let descriptor = temporaryPath.withCString {
            Darwin.open($0, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, mode_t(S_IRUSR | S_IWUSR))
        }
        guard descriptor >= 0 else { throw FixtureFailure.snapshotWriteFailed }
        var renamed = false
        defer {
            _ = Darwin.close(descriptor)
            if !renamed { _ = temporaryPath.withCString { Darwin.unlink($0) } }
        }
        guard Darwin.fchmod(descriptor, mode_t(S_IRUSR | S_IWUSR)) == 0 else {
            throw FixtureFailure.snapshotWriteFailed
        }

        try data.withUnsafeBytes { buffer in
            guard let baseAddress = buffer.baseAddress else { throw FixtureFailure.snapshotWriteFailed }
            var offset = 0
            while offset < buffer.count {
                let written = Darwin.write(descriptor, baseAddress.advanced(by: offset), buffer.count - offset)
                if written < 0 && errno == EINTR { continue }
                guard written > 0 else { throw FixtureFailure.snapshotWriteFailed }
                offset += written
            }
        }
        guard Darwin.fsync(descriptor) == 0 else { throw FixtureFailure.snapshotWriteFailed }
        let renameResult = temporaryPath.withCString { source in
            path.withCString { destination in Darwin.rename(source, destination) }
        }
        guard renameResult == 0 else { throw FixtureFailure.snapshotWriteFailed }
        renamed = true

        let directoryDescriptor = parent.withCString { Darwin.open($0, O_RDONLY | O_DIRECTORY) }
        if directoryDescriptor >= 0 {
            _ = Darwin.fsync(directoryDescriptor)
            _ = Darwin.close(directoryDescriptor)
        }
    }
}
