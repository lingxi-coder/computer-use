import ApplicationServices
import Foundation

func runOwnedTransientInventoryTests() throws -> Int {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw NSError(domain: "OwnedTransientInventoryTests", code: passed + 1) }
        passed += 1
    }
    let array = UInt64(CFArrayGetTypeID()), boolean = UInt64(CFBooleanGetTypeID())
    try check(ownedSheetInventoryDisposition(error: AXError.attributeUnsupported.rawValue,
        typeID: nil, rawCount: nil, convertedCount: nil, timely: true) == .unsupportedRequiresTree)
    try check(ownedSheetInventoryDisposition(error: AXError.noValue.rawValue,
        typeID: nil, rawCount: nil, convertedCount: nil, timely: true) == .empty)
    try check(ownedSheetInventoryDisposition(error: 0, typeID: array,
        rawCount: 0, convertedCount: 0, timely: true) == .empty)
    for error in [AXError.cannotComplete.rawValue, AXError.failure.rawValue, AXError.apiDisabled.rawValue] {
        try check(ownedSheetInventoryDisposition(error: error, typeID: nil,
            rawCount: nil, convertedCount: nil, timely: true) == nil)
        try check(!ownedNodeIsNonTransient(role: kAXButtonRole, modalError: error,
            modalTypeID: nil, modalValue: nil, timely: true))
    }
    for error in [AXError.attributeUnsupported.rawValue, AXError.noValue.rawValue] {
        try check(ownedSheetInventoryDisposition(error: error, typeID: array,
            rawCount: 0, convertedCount: 0, timely: true) == nil)
        try check(ownedNodeIsNonTransient(role: kAXButtonRole, modalError: error,
            modalTypeID: nil, modalValue: nil, timely: true))
        try check(!ownedNodeIsNonTransient(role: kAXWindowRole, modalError: error,
            modalTypeID: nil, modalValue: nil, timely: true))
    }
    try check(ownedSheetInventoryDisposition(error: 0, typeID: array,
        rawCount: 1, convertedCount: 1, timely: true) == nil)
    try check(ownedSheetInventoryDisposition(error: 0, typeID: array,
        rawCount: 0, convertedCount: nil, timely: true) == nil)
    try check(ownedSheetInventoryDisposition(error: 0, typeID: array,
        rawCount: 0, convertedCount: 0, timely: false) == nil)
    for role in [kAXSheetRole, "AXDialog", "AXPopover", kAXMenuRole] {
        try check(!ownedNodeIsNonTransient(role: role, modalError: 0,
            modalTypeID: boolean, modalValue: false, timely: true))
    }
    try check(ownedNodeIsNonTransient(role: kAXWindowRole, modalError: 0,
        modalTypeID: boolean, modalValue: false, timely: true))
    try check(!ownedNodeIsNonTransient(role: kAXButtonRole, modalError: 0,
        modalTypeID: boolean, modalValue: true, timely: true))
    try check(!ownedNodeIsNonTransient(role: kAXButtonRole, modalError: 0,
        modalTypeID: UInt64(CFStringGetTypeID()), modalValue: false, timely: true))
    try check(!ownedNodeIsNonTransient(role: kAXButtonRole, modalError: 0,
        modalTypeID: boolean, modalValue: nil, timely: true))
    try check(!ownedNodeIsNonTransient(role: kAXButtonRole, modalError: 0,
        modalTypeID: boolean, modalValue: false, timely: false))
    return passed
}
