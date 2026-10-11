import Foundation

func runOwnedDocumentBootstrapTests() throws -> [String] {
    let fields: [String: Any] = [
        "version": 1, "registration_id": "6e653e8d-fdb1-4205-8ef7-df890c83879b",
        "pid": 42, "start_seconds": 1_700_000_000, "start_micros": 100,
        "executable_path": "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit",
        "bundle_id": "com.apple.TextEdit", "document_path": "/private/tmp/owned-test/document.txt",
        "document_sha256": String(repeating: "a", count: 64), "document_dev": 1, "document_ino": 2,
        "directory_dev": 1, "directory_ino": 3, "issued_unix_ms": 10_000,
    ]
    func data(_ values: [String: Any]) throws -> Data {
        try JSONSerialization.data(withJSONObject: values, options: [.sortedKeys])
    }
    func rejected(_ values: [String: Any], now: UInt64 = 10_100) -> Bool {
        do { _ = try OwnedDocumentBootstrap.decode(data(values), nowMS: now); return false }
        catch { return true }
    }
    let bytes = try data(fields)
    let value = try OwnedDocumentBootstrap.decode(bytes, nowMS: 10_100)
    try requireTest(value.pid == 42 && value.document_ino == 2, "private owned bootstrap exact payload")
    var tests = ["private owned bootstrap exact payload"]
    for (key, replacement) in [
        ("version", 2 as Any), ("pid", 0 as Any), ("pid", 2_147_483_648 as Any),
        ("start_micros", 1_000_000 as Any), ("bundle_id", "unregistered.app" as Any),
        ("executable_path", "/tmp/foreign" as Any), ("document_path", "relative/document.txt" as Any),
        ("document_path", "/private/tmp/owned-test/foreign.txt" as Any),
        ("document_sha256", "invalid" as Any), ("document_ino", 0 as Any),
        ("directory_ino", 9_007_199_254_740_992 as Any),
    ] {
        var changed = fields; changed[key] = replacement
        try requireTest(rejected(changed), "private owned bootstrap rejects invalid \(key)")
    }
    var unknown = fields; unknown["privateTemporaryDocument"] = true
    var missing = fields; missing.removeValue(forKey: "directory_ino")
    try requireTest(rejected(unknown) && rejected(missing), "private owned bootstrap exact keys")
    tests.append("private owned bootstrap exact keys and bounds")
    try requireTest(rejected(fields, now: 15_001) && rejected(fields, now: 8_999),
                    "private owned bootstrap rejects stale and future issuance")
    tests.append("private owned bootstrap issuance freshness")
    let pipe = Pipe()
    let count = UInt32(bytes.count)
    var frame = Data([UInt8((count >> 24) & 255), UInt8((count >> 16) & 255), UInt8((count >> 8) & 255), UInt8(count & 255)])
    frame.append(bytes); frame.append(Data("REQUEST".utf8))
    try pipe.fileHandleForWriting.write(contentsOf: frame)
    try pipe.fileHandleForWriting.close()
    let parsed = try OwnedDocumentBootstrap.read(from: pipe.fileHandleForReading, nowMS: 10_100)
    let rest = try pipe.fileHandleForReading.readToEnd()
    try requireTest(parsed.registration_id == value.registration_id && rest == Data("REQUEST".utf8),
                    "private owned prefix consumes only its bounded frame")
    try pipe.fileHandleForReading.close()
    tests.append("private owned prefix preserves following request")
    for bad in [Data([0, 0, 0, 0]), Data([0, 1, 0, 0]), Data([0, 0, 0, 2, 123])] {
        let pipe = Pipe()
        try pipe.fileHandleForWriting.write(contentsOf: bad)
        try pipe.fileHandleForWriting.close()
        var failed = false
        do { _ = try OwnedDocumentBootstrap.read(from: pipe.fileHandleForReading, nowMS: 10_100) }
        catch { failed = true }
        try pipe.fileHandleForReading.close()
        try requireTest(failed, "private owned prefix rejects invalid length or truncation")
    }
    tests.append("private owned prefix length and truncation")
    return tests
}
