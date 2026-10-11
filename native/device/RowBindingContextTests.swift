import ApplicationServices
import CoreFoundation
import Foundation

private func rowBatchTestArray(_ values: [CFTypeRef]) -> CFArray {
    NSArray(array: values) as CFArray
}

private func rowBatchTestSlotIsAbsent(_ slots: [NativeRowBatchSlot]?, index: Int) -> Bool {
    guard let slots, slots.indices.contains(index) else { return false }
    if case .absent = slots[index] { return true }
    return false
}

private func rowBatchTestSlotString(_ slots: [NativeRowBatchSlot]?, index: Int) -> String? {
    guard let slots, slots.indices.contains(index), case .value(let raw) = slots[index],
          CFGetTypeID(raw) == CFStringGetTypeID() else { return nil }
    return raw as? String
}

private func rowBatchTestDecodeFails(error: AXError, values: CFArray?,
                                     attributes: [NativeRowBatchAttribute]) -> Bool {
    do {
        _ = try NativeRowBindingValidation.decodeBatchResponse(error: error,
            values: values, attributes: attributes)
        return false
    } catch {
        return true
    }
}

/// Pure validation tests only. AXUIElement relationship equality remains in
/// the real worker reader and is never simulated here.
func runRowBindingContextTests(startingAt offset: Int) throws -> Int {
    var passed = 0
    func check(_ condition: Bool) throws {
        guard condition else { throw DeviceTestFailure(check: offset + passed + 1) }
        passed += 1
    }

    let rawEnabled: CFTypeRef = kCFBooleanTrue
    let rawDisabled: CFTypeRef = kCFBooleanFalse
    let numericOne: CFTypeRef = NSNumber(value: Int32(1))
    let wrongType: CFTypeRef = "true" as CFString
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: rawEnabled, withinDeadline: true) == .enabled)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: rawDisabled, withinDeadline: true) == .disabled)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: numericOne, withinDeadline: true) == .unavailable)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: wrongType, withinDeadline: true) == .unavailable)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: nil, withinDeadline: true) == .unavailable)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .attributeUnsupported,
        value: nil, withinDeadline: true) == .attributeUnsupported)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .attributeUnsupported,
        value: rawEnabled, withinDeadline: true) == .attributeUnsupported)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .noValue,
        value: nil, withinDeadline: true) == .unavailable)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .cannotComplete,
        value: nil, withinDeadline: true) == .unavailable)
    try check(NativeRowBindingValidation.axEnabledObservation(error: .success,
        value: rawEnabled, withinDeadline: false) == .unavailable)
    try check(NativeRowBindingValidation.rowEnabledStatesMatch(table: .enabled, target: .enabled,
        expectedTable: .enabled, expectedTarget: .enabled))
    try check(NativeRowBindingValidation.rowEnabledStatesMatch(table: .attributeUnsupported,
        target: .enabled, expectedTable: .attributeUnsupported, expectedTarget: .enabled))
    try check(!NativeRowBindingValidation.rowEnabledStatesMatch(table: .attributeUnsupported,
        target: .enabled, expectedTable: .enabled, expectedTarget: .enabled))
    try check(!NativeRowBindingValidation.rowEnabledStatesMatch(table: .disabled, target: .enabled,
        expectedTable: .enabled, expectedTarget: .enabled))
    try check(!NativeRowBindingValidation.rowEnabledStatesMatch(table: .disabled, target: .enabled,
        expectedTable: .disabled, expectedTarget: .enabled))
    try check(!NativeRowBindingValidation.rowEnabledStatesMatch(table: .unavailable, target: .enabled,
        expectedTable: .unavailable, expectedTarget: .enabled))

    let actualGlobalIndices: [UInt32] = [42, 511]
    try check(NativeRowBindingValidation.visibleIndicesMatch(actualGlobalIndices,
        target: 42, expected: actualGlobalIndices))
    try check(!NativeRowBindingValidation.visibleIndicesMatch(actualGlobalIndices,
        target: 0, expected: actualGlobalIndices))
    try check(!NativeRowBindingValidation.visibleIndicesMatch(actualGlobalIndices,
        target: 42, expected: [0, 1]))
    try check(!NativeRowBindingValidation.visibleIndicesMatch([42, 42], target: 42, expected: nil))
    try check(!NativeRowBindingValidation.visibleIndicesMatch([], target: 42, expected: nil))
    try check(!NativeRowBindingValidation.visibleIndicesMatch(
        Array(0..<UInt32(NativeRowBindingValidation.maximumVisibleRows + 1)), target: 0, expected: nil))
    try check(NativeRowBindingValidation.uniqueSelectorMatch([42], target: 42))
    try check(!NativeRowBindingValidation.uniqueSelectorMatch([42, 511], target: 42))
    try check(!NativeRowBindingValidation.uniqueSelectorMatch([511], target: 42))
    try check(!NativeRowBindingValidation.uniqueSelectorMatch([], target: 42))

    let maximumIndex: CFTypeRef = NSNumber(value: UInt32.max)
    let overflowIndex: CFTypeRef = NSNumber(value: UInt64(UInt32.max) + 1)
    let fractionalIndex: CFTypeRef = NSNumber(value: 42.5)
    let booleanIndex: CFTypeRef = NSNumber(value: true)
    let negativeIndex: CFTypeRef = NSNumber(value: -1)
    try check(NativeRowBindingValidation.rowAXIndex(maximumIndex) == UInt32.max)
    try check(NativeRowBindingValidation.rowAXIndex(overflowIndex) == nil)
    try check(NativeRowBindingValidation.rowAXIndex(fractionalIndex) == nil)
    try check(NativeRowBindingValidation.rowAXIndex(booleanIndex) == nil)
    try check(NativeRowBindingValidation.rowAXIndex(negativeIndex) == nil)

    try check(NativeRowBindingValidation.singletonRowIndexRange(location: 42, length: 1) == 42)
    try check(NativeRowBindingValidation.singletonRowIndexRange(location: Int(UInt32.max), length: 1) == UInt32.max)
    try check(NativeRowBindingValidation.singletonRowIndexRange(location: -1, length: 1) == nil)
    try check(NativeRowBindingValidation.singletonRowIndexRange(location: 42, length: 2) == nil)
    if Int.bitWidth > 32 {
        try check(NativeRowBindingValidation.singletonRowIndexRange(location: Int(UInt32.max) + 1, length: 1) == nil)
    }
    try check(NativeRowBindingValidation.singletonColumnIndexRange(location: 63, length: 1) == 63)
    try check(NativeRowBindingValidation.singletonColumnIndexRange(location: 64, length: 1) == nil)
    try check(NativeRowBindingValidation.singletonColumnIndexRange(location: 3, length: 2) == nil)

    var rowRange = CFRange(location: 42, length: 1)
    let rowRangeValue = AXValueCreate(.cfRange, &rowRange)
    var mergedRowRange = CFRange(location: 42, length: 2)
    let mergedRowRangeValue = AXValueCreate(.cfRange, &mergedRowRange)
    var columnRange = CFRange(location: 63, length: 1)
    let columnRangeValue = AXValueCreate(.cfRange, &columnRange)
    try check(rowRangeValue.map { NativeRowBindingValidation.rowIndexRange($0) == 42 } ?? false)
    try check(mergedRowRangeValue.map { NativeRowBindingValidation.rowIndexRange($0) == nil } ?? false)
    try check(columnRangeValue.map { NativeRowBindingValidation.columnIndexRange($0) == 63 } ?? false)

    try check(NativeRowBindingValidation.validBoundedListCount(1, limit: 256))
    try check(NativeRowBindingValidation.validBoundedListCount(256, limit: 256))
    try check(!NativeRowBindingValidation.validBoundedListCount(0, limit: 256))
    try check(!NativeRowBindingValidation.validBoundedListCount(257, limit: 256))
    try check(NativeRowBindingValidation.validBoundedListCount(64, limit: 64))
    try check(!NativeRowBindingValidation.validBoundedListCount(65, limit: 64))
    try check(NativeRowBindingValidation.validBoundedListCount(128, limit: 128))
    try check(!NativeRowBindingValidation.validBoundedListCount(129, limit: 128))
    try check(NativeRowBindingValidation.rowCellListLimit(currentTotal: 0) == 128)
    try check(NativeRowBindingValidation.rowCellListLimit(currentTotal: 2_040) == 8)
    try check(NativeRowBindingValidation.rowCellListLimit(currentTotal: 2_048) == 0)
    try check(!NativeRowBindingValidation.validBoundedListCount(1,
        limit: NativeRowBindingValidation.rowCellListLimit(currentTotal: 2_048) ?? -1))
    try check(NativeRowBindingValidation.rowCellListLimit(currentTotal: -1) == nil)
    try check(NativeRowBindingValidation.rowCellListLimit(currentTotal: 2_049) == nil)

    var multiRowTotal = 0
    var multiRowTotalValid = true
    for _ in 0..<16 {
        guard let nextTotal = NativeRowBindingValidation.accumulatedCellCount(
            currentTotal: multiRowTotal, rowCount: 128) else {
            multiRowTotalValid = false
            break
        }
        multiRowTotal = nextTotal
    }
    try check(multiRowTotalValid && multiRowTotal == 2_048)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: 2_040, rowCount: 8) == 2_048)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: 2_040, rowCount: 9) == nil)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: 2_048, rowCount: 1) == nil)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: -1, rowCount: 1) == nil)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: 1, rowCount: -1) == nil)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: Int.max, rowCount: Int.max) == nil)
    try check(NativeRowBindingValidation.accumulatedCellCount(
        currentTotal: 0, rowCount: 0) == nil)
    try check(NativeRowBindingValidation.validBoundedListCount(8, limit: 8))
    try check(!NativeRowBindingValidation.validBoundedListCount(9, limit: 8))
    try check(NativeRowBindingValidation.callTimeoutMilliseconds(now: 10, deadline: 100) == 25)
    try check(NativeRowBindingValidation.callTimeoutMilliseconds(now: 91, deadline: 100) == 9)
    try check(NativeRowBindingValidation.callTimeoutMilliseconds(now: 100, deadline: 100) == nil)

    let booleanValue: CFTypeRef = NSNumber(value: true)
    let numericValue: CFTypeRef = NSNumber(value: 42.25)
    let nonFiniteValue: CFTypeRef = NSNumber(value: Double.nan)
    let textValue: CFTypeRef = "alpha" as CFString
    try check(NativeRowSelectorValue.fromAXValue(booleanValue) == .boolean(true))
    try check(NativeRowSelectorValue.fromAXValue(numericValue) == .number(42.25))
    try check(NativeRowSelectorValue.fromAXValue(nonFiniteValue) == nil)
    try check(NativeRowSelectorValue.fromAXValue(textValue) == .text("alpha"))
    try check(NativeRowSelectorValue.text(String(repeating: "x", count: 2048)).valid)
    try check(!NativeRowSelectorValue.text(String(repeating: "x", count: 2049)).valid)
    try check(!NativeRowSelectorValue.text("   ").valid)
    try check(!NativeRowSelectorValue.number(1.0e12 + 1).valid)

    let name = NativeRowSelectorCondition(columnAXIndex: 2, headerLabel: "Name", value: .text("alpha"))
    let status = NativeRowSelectorCondition(columnAXIndex: 5, headerLabel: "Status", value: .boolean(true))
    try check(NativeRowBindingValidation.validSelectorConditions([name, status]))
    try check(!NativeRowBindingValidation.validSelectorConditions([name,
        NativeRowSelectorCondition(columnAXIndex: 2, headerLabel: "Alias", value: .text("alpha"))]))
    try check(!NativeRowBindingValidation.validSelectorConditions(Array(repeating: name,
        count: NativeRowBindingValidation.maximumSelectorConditions + 1)))
    let boundedConditions = (0..<NativeRowBindingValidation.maximumSelectorConditions).map { index in
        NativeRowSelectorCondition(columnAXIndex: UInt8(index), headerLabel: "Column \(index)",
            value: .text("v\(index)"))
    }
    try check(NativeRowBindingValidation.validSelectorConditions(boundedConditions))
    try check(!NativeRowBindingValidation.validSelectorConditions([
        NativeRowSelectorCondition(columnAXIndex: 2, headerLabel: "   ", value: .text("alpha"))]))
    try check(!NativeRowBindingValidation.validSelectorConditions([
        NativeRowSelectorCondition(columnAXIndex: 2, headerLabel: "Name", value: .number(.infinity))]))
    try check(!NativeRowBindingValidation.validSelectorConditions([
        NativeRowSelectorCondition(columnAXIndex: 64, headerLabel: "Name", value: .text("alpha"))]))
    try check(!NativeRowBindingValidation.validSelectorConditions([name,
        NativeRowSelectorCondition(columnAXIndex: 3, headerLabel: " name ", value: .text("alpha"))]))
    try check(!NativeRowBindingValidation.validSelectorConditions([
        NativeRowSelectorCondition(columnAXIndex: 2, headerLabel: String(repeating: "x", count: 257),
            value: .text("alpha"))]))

    let requiredRole = NativeRowBatchAttribute.required("AXRole")
    let optionalSubrole = NativeRowBatchAttribute.optional("AXSubrole")
    let roleValue: CFTypeRef = "AXCell" as CFString
    let missingValue: CFTypeRef = kCFNull
    let optionalBatch = try NativeRowBindingValidation.decodeBatchResponse(error: .success,
        values: rowBatchTestArray([roleValue, missingValue]),
        attributes: [requiredRole, optionalSubrole])
    try check(rowBatchTestSlotString(optionalBatch, index: 0) == "AXCell")
    try check(rowBatchTestSlotIsAbsent(optionalBatch, index: 1))
    try check(rowBatchTestDecodeFails(error: .success,
        values: rowBatchTestArray([missingValue]), attributes: [requiredRole]))
    try check(rowBatchTestDecodeFails(error: .success, values: nil, attributes: [requiredRole]))
    try check(rowBatchTestDecodeFails(error: .success,
        values: rowBatchTestArray([roleValue, missingValue, roleValue]),
        attributes: [requiredRole, optionalSubrole]))
    try check(rowBatchTestDecodeFails(error: .cannotComplete,
        values: rowBatchTestArray([roleValue]), attributes: [requiredRole]))
    try check(rowBatchTestDecodeFails(error: .noValue,
        values: rowBatchTestArray([roleValue]), attributes: [requiredRole]))

    var missingAXError = AXError.noValue
    var unsupportedAXError = AXError.attributeUnsupported
    var timeoutAXError = AXError.cannotComplete
    let missingAXValue = AXValueCreate(.axError, &missingAXError)
    let unsupportedAXValue = AXValueCreate(.axError, &unsupportedAXError)
    let timeoutAXValue = AXValueCreate(.axError, &timeoutAXError)
    try check(missingAXValue.map { errorValue in
        let decoded = try? NativeRowBindingValidation.decodeBatchResponse(error: .success,
            values: rowBatchTestArray([errorValue]), attributes: [optionalSubrole])
        return rowBatchTestSlotIsAbsent(decoded, index: 0)
    } ?? false)
    try check(unsupportedAXValue.map { errorValue in
        let decoded = try? NativeRowBindingValidation.decodeBatchResponse(error: .success,
            values: rowBatchTestArray([errorValue]), attributes: [optionalSubrole])
        return rowBatchTestSlotIsAbsent(decoded, index: 0)
    } ?? false)
    try check(timeoutAXValue.map { errorValue in
        rowBatchTestDecodeFails(error: .success, values: rowBatchTestArray([errorValue]),
            attributes: [optionalSubrole])
    } ?? false)
    try check(missingAXValue.map { errorValue in
        rowBatchTestDecodeFails(error: .success, values: rowBatchTestArray([errorValue]),
            attributes: [requiredRole])
    } ?? false)
    try check(rowBatchTestDecodeFails(error: .success,
        values: rowBatchTestArray([roleValue, roleValue]), attributes: [requiredRole]))
    try check(rowBatchTestDecodeFails(error: .success, values: nil,
        attributes: Array(repeating: requiredRole,
            count: NativeRowBindingValidation.maximumBatchAttributes + 1)))
    try check(rowBatchTestDecodeFails(error: .success, values: rowBatchTestArray([roleValue]),
        attributes: [requiredRole, NativeRowBatchAttribute.required("AXRole")]))

    return passed
}
