import Foundation
import Security
import LocalAuthentication
import CryptoKit

private let service = "com.lingxi.computer-user.model-keys"
private let testService = "com.lingxi.computer-user.model-keys.self-test"
private let limit = 32 * 1024

private struct Request: Decodable {
    var operation: String
    var account: String
    var key: String?
}
private struct Reply: Encodable { var ok: Bool; var code: String; var key: String? = nil; var tests: Int? = nil }
private enum Failure: Error { case invalid; case status(OSStatus) }

private func safeFailureCode(_ error: Error) -> String {
    guard let failure = error as? Failure else { return "unavailable" }
    switch failure {
    case .invalid:
        return "invalid"
    case .status(let status):
        if status == errSecInteractionNotAllowed || status == errSecAuthFailed { return "auth_required" }
        if status == errSecNotAvailable { return "locked" }
        return "unavailable"
    }
}

private func query(_ namespace: String, _ account: String) -> [String: Any] {
    let context = LAContext()
    context.interactionNotAllowed = true
    return [kSecClass as String: kSecClassGenericPassword,
        kSecAttrService as String: namespace, kSecAttrAccount as String: account,
        kSecAttrSynchronizable as String: false,
        kSecUseAuthenticationContext as String: context]
}
private func validAccount(_ value: String) -> Bool {
    value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
}
private func save(_ namespace: String, _ account: String, _ key: String) throws {
    guard validAccount(account), !key.isEmpty, key.utf8.count <= 16 * 1024,
        !key.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains) else { throw Failure.invalid }
    let q = query(namespace, account)
    var secret = Data(key.utf8)
    defer { secret.resetBytes(in: 0..<secret.count) }
    let attributes: [String: Any] = [kSecValueData as String: secret]
    let updated = SecItemUpdate(q as CFDictionary, attributes as CFDictionary)
    if updated == errSecItemNotFound {
        var add = q
        add[kSecValueData as String] = secret
        add[kSecAttrLabel as String] = "Computer Use model connection"
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else { throw Failure.status(status) }
    } else if updated != errSecSuccess { throw Failure.status(updated) }
}
private func load(_ namespace: String, _ account: String) throws -> String? {
    guard validAccount(account) else { throw Failure.invalid }
    var q = query(namespace, account)
    q[kSecReturnData as String] = true
    q[kSecMatchLimit as String] = kSecMatchLimitOne
    var result: CFTypeRef?
    let status = SecItemCopyMatching(q as CFDictionary, &result)
    if status == errSecItemNotFound { return nil }
    guard status == errSecSuccess else { throw Failure.status(status) }
    guard var bytes = result as? Data, bytes.count <= 16 * 1024 else { throw Failure.invalid }
    defer { bytes.resetBytes(in: 0..<bytes.count) }
    guard let key = String(data: bytes, encoding: .utf8), !key.isEmpty else { throw Failure.invalid }
    return key
}
private func forget(_ namespace: String, _ account: String) throws {
    guard validAccount(account) else { throw Failure.invalid }
    let status = SecItemDelete(query(namespace, account) as CFDictionary)
    guard status == errSecSuccess || status == errSecItemNotFound else { throw Failure.status(status) }
}
private func selfTest() throws -> Reply {
    let account = SHA256.hash(data: Data(UUID().uuidString.utf8)).map { String(format: "%02x", $0) }.joined()
    defer { try? forget(testService, account) }
    try forget(testService, account)
    guard try load(testService, account) == nil else { throw Failure.invalid }
    try save(testService, account, "computer-use-owned-test-one")
    guard try load(testService, account) == "computer-use-owned-test-one" else { throw Failure.invalid }
    try save(testService, account, "computer-use-owned-test-two")
    guard try load(testService, account) == "computer-use-owned-test-two" else { throw Failure.invalid }
    try forget(testService, account)
    guard try load(testService, account) == nil else { throw Failure.invalid }
    guard !validAccount("../../other-service"), !validAccount(String(repeating: "A", count: 64)) else { throw Failure.invalid }
    return Reply(ok: true, code: "owned_keychain_roundtrip", tests: 7)
}
private func input() throws -> Request {
    var bytes = Data()
    defer { bytes.resetBytes(in: 0..<bytes.count) }
    while let chunk = try FileHandle.standardInput.read(upToCount: 4096), !chunk.isEmpty {
        guard bytes.count + chunk.count <= limit else { throw Failure.invalid }
        bytes.append(chunk)
    }
    guard let object = try JSONSerialization.jsonObject(with: bytes) as? [String: Any],
        Set(object.keys).isSubset(of: ["operation", "account", "key"]), object["operation"] != nil, object["account"] != nil else { throw Failure.invalid }
    return try JSONDecoder().decode(Request.self, from: bytes)
}

@main private enum KeychainMain {
    static func main() {
        var reply: Reply
        do {
            if CommandLine.arguments.dropFirst() == ["--self-test"] { reply = try selfTest() }
            else {
                guard CommandLine.arguments.count == 1 else { throw Failure.invalid }
                var request = try input()
                defer { request.key = nil }
                guard validAccount(request.account) else { throw Failure.invalid }
                switch request.operation {
                case "save": guard let key = request.key else { throw Failure.invalid }; try save(service, request.account, key); reply = Reply(ok: true, code: "saved")
                case "load":
                    guard request.key == nil else { throw Failure.invalid }
                    if let key = try load(service, request.account) { reply = Reply(ok: true, code: "loaded", key: key) }
                    else { reply = Reply(ok: true, code: "not_found") }
                case "forget": guard request.key == nil else { throw Failure.invalid }; try forget(service, request.account); reply = Reply(ok: true, code: "forgotten")
                default: throw Failure.invalid
                }
            }
        } catch { reply = Reply(ok: false, code: safeFailureCode(error)) }
        // No NSError description, request, account or input is echoed on failure.
        if let data = try? JSONEncoder().encode(reply) { FileHandle.standardOutput.write(data); FileHandle.standardOutput.write(Data([10])) }
        if !reply.ok { exit(1) }
    }
}
