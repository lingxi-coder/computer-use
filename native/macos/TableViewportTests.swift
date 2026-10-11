import Foundation
import CoreGraphics
import ApplicationServices

/// Offline checks for the value-only table viewport projection. These tests do
/// not query Accessibility, infer rows from geometry, or exercise input.
func runTableViewportTests() throws -> [String] {
    var actualColumnRange = CFRange(location: 2, length: 1)
    let columnRange = AXValueCreate(.cfRange, &actualColumnRange)
    try requireTest(observedTableColumnIndex(columnRange, cell: true) == 2,
        "actual single-column AX range supplies its column index")
    var spanningRange = CFRange(location: 1, length: 2)
    try requireTest(observedTableColumnIndex(AXValueCreate(.cfRange, &spanningRange), cell: true) == nil,
        "merged column range remains unknown")
    for range in [CFRange(location: -1, length: 1), CFRange(location: 64, length: 1)] {
        var invalidRange = range
        try requireTest(observedTableColumnIndex(AXValueCreate(.cfRange, &invalidRange), cell: true) == nil,
            "negative and out-of-range column range locations remain unknown")
    }
    for number in [NSNumber(value: true), NSNumber(value: -1), NSNumber(value: 64), NSNumber(value: 1.5)] {
        try requireTest(observedTableColumnIndex(number, cell: false) == nil,
            "AX column index rejects boolean, out-of-range and nonintegral values")
    }
    try requireTest(observedTableColumnIndex(nil, cell: true) == nil,
        "missing cell AXColumnIndexRange remains unknown")
    for number in [NSNumber(value: true), NSNumber(value: -1), NSNumber(value: 1.5),
                   NSNumber(value: UInt64(UInt32.max) + 1)] {
        try requireTest(observedTableColumnIndex(number, cell: true) == nil,
            "cell column range rejects non-AXValue, boolean, fractional, and overflow inputs")
    }
    try requireTest(observedTableColumnIndex(NSNumber(value: 1), cell: false) == 1,
        "actual integral AX column index is preserved")
    try requireTest(observedTableRowIndex(nil) == nil,
        "missing actual row AXIndex remains unknown")
    try requireTest(observedTableRowIndex(NSNumber(value: 42)) == 42,
        "actual row AXIndex is preserved independently of visible ordinal")
    try requireTest(observedTableRowIndex(NSNumber(value: UInt64(UInt32.max))) == UInt32.max,
        "row AXIndex accepts the full UInt32 range")
    for number in [NSNumber(value: true), NSNumber(value: -1), NSNumber(value: 42.5),
                   NSNumber(value: UInt64(UInt32.max) + 1)] {
        try requireTest(observedTableRowIndex(number) == nil,
            "row AXIndex rejects boolean, negative, fractional, and overflow values")
    }
    try requireTest(observedTableRowIndexRange(nil) == nil
                    && observedTableRowIndexRange(NSNumber(value: 42)) == nil,
                    "missing or non-AXValue row ranges remain unknown")
    for number in [NSNumber(value: true), NSNumber(value: 42.5),
                   NSNumber(value: UInt64(UInt32.max) + 1)] {
        try requireTest(observedTableRowIndexRange(number) == nil,
            "row range rejects boolean, fractional, and overflowing non-AXValue inputs")
    }
    var actualRowRange = CFRange(location: 42, length: 1)
    let rowRange = AXValueCreate(.cfRange, &actualRowRange)
    try requireTest(observedTableRowIndexRange(rowRange) == 42,
        "actual singleton AXRowIndexRange location is preserved")
    for range in [CFRange(location: 42, length: 2), CFRange(location: -1, length: 1),
                  CFRange(location: Int(UInt32.max) + 1, length: 1)] {
        var invalidRange = range
        try requireTest(observedTableRowIndexRange(AXValueCreate(.cfRange, &invalidRange)) == nil,
            "merged, negative, and overflow row ranges remain unknown")
    }
    let nonBooleanEnabled = NSNumber(value: 1) as CFTypeRef
    try requireTest(classifyTableAXEnabled(error: .success, value: kCFBooleanTrue) == .enabled
                    && classifyTableAXEnabled(error: .success, value: kCFBooleanFalse) == .disabled,
                    "AXEnabled requires and preserves a genuine Boolean value")
    try requireTest(classifyTableAXEnabled(error: .attributeUnsupported, value: kCFBooleanTrue)
                    == .attributeUnsupported,
                    "only AXError.attributeUnsupported gets its own explicit state")
    try requireTest(classifyTableAXEnabled(error: .noValue, value: nil) == .unavailable
                    && classifyTableAXEnabled(error: .cannotComplete, value: nil) == .unavailable
                    && classifyTableAXEnabled(error: .success, value: nil) == .unavailable
                    && classifyTableAXEnabled(error: .success, value: nonBooleanEnabled) == .unavailable,
                    "missing, failed, and non-Boolean AXEnabled reads remain unavailable")
    let scrollFrame = Rect(CGRect(x: 10, y: 20, width: 300, height: 100))
    let tableFrame = Rect(CGRect(x: 10, y: 20, width: 300, height: 4400))
    let viewportFrame = Rect(CGRect(x: 10, y: 20, width: 300, height: 100))
    let scroll = AXTableNodeContext(instanceRef: "scroll-ref", parentInstanceRef: nil,
        processID: 42, role: "AXScrollArea", frame: scrollFrame)
    let table = AXTableNodeContext(instanceRef: "table-ref", parentInstanceRef: "scroll-ref",
        processID: 42, role: "AXTable", frame: tableFrame)
    let column = AXTableColumnContext(instanceRef: "column-ref", parentInstanceRef: "table-ref",
        processID: 42, role: "AXColumn", axIndex: 2,
        frame: Rect(CGRect(x: 10, y: 20, width: 150, height: 30)),
        headerInstanceRef: "header-ref", headerProcessID: 42, header: "Name")
    let firstCell = AXTableCellContext(instanceRef: "cell-ref-1", parentInstanceRef: "row-ref-1",
        processID: 42, role: "AXCell", frame: Rect(CGRect(x: 10, y: 20, width: 150, height: 30)),
        rowAXIndex: 42, columnAXIndex: 2,
        columnInstanceRef: "column-ref", headerInstanceRef: "header-ref", headerProcessID: 42,
        columnHeader: "Name", value: .text("Weekly report"))
    let firstRow = AXTableRowContext(instanceRef: "row-ref-1", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: Rect(CGRect(x: 10, y: 20, width: 300, height: 30)),
        axIndex: 42,
        accessibilityIdentifier: "opaque-row-1", axEnabled: .attributeUnsupported, selected: nil, selectedSettable: nil, cellsComplete: true,
        cells: [firstCell])

    guard let partial = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow], visibleRowsComplete: false,
        columns: [column], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: actual partial AXVisibleRows context should project")
    }
    try requireTest(partial.visible_rows_complete == false
                    && partial.ax_enabled == .attributeUnsupported
                    && partial.visible_rows[0].ax_enabled == .attributeUnsupported
                    && partial.visible_rows[0].selected == nil
                    && partial.visible_rows[0].accessibility_identifier == "opaque-row-1"
                    && partial.visible_rows[0].ax_index == 42
                    && partial.visible_rows[0].cells[0].value == .text("Weekly report")
                    && partial.visible_rows[0].cells[0].row_ax_index == 42
                    && partial.visible_rows[0].cells[0].column_ax_index == 2
                    && partial.columns[0].ax_index == 2,
                    "actual row index 42 at visible ordinal zero, cell ranges, and partial facts stay explicit")

    let contradictoryCell = AXTableCellContext(instanceRef: "mismatched-cell", parentInstanceRef: "row-ref-1",
        processID: 42, role: "AXCell", frame: firstCell.frame,
        rowAXIndex: 43, columnAXIndex: 2,
        columnInstanceRef: "column-ref", headerInstanceRef: "header-ref", headerProcessID: 42,
        columnHeader: "Name", value: .text("Weekly report"))
    let contradictoryRow = AXTableRowContext(instanceRef: "row-ref-1", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: firstRow.frame, axIndex: 42,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: true,
        cells: [contradictoryCell])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [contradictoryRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "conflicting actual AXRow and AXRowIndexRange facts reject the table projection")

    let unknownRowIndex = AXTableRowContext(instanceRef: "row-ref-1", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: firstRow.frame, axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: true,
        cells: [firstCell])
    guard let unknownIndexProjection = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [unknownRowIndex], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: row index must remain unknown when only its cell range is known")
    }
    try requireTest(unknownIndexProjection.visible_rows[0].ax_index == nil
                    && unknownIndexProjection.visible_rows[0].cells[0].row_ax_index == 42,
                    "cell row range never backfills the independently observed AXRow AXIndex")

    let mismatchedColumnCell = AXTableCellContext(instanceRef: "mismatched-column-cell",
        parentInstanceRef: "mismatched-column-row", processID: 42, role: "AXCell",
        frame: firstCell.frame, rowAXIndex: 42, columnAXIndex: 3,
        columnInstanceRef: "column-ref", headerInstanceRef: "header-ref", headerProcessID: 42,
        columnHeader: "Name", value: .text("Weekly report"))
    let mismatchedColumnRow = AXTableRowContext(instanceRef: "mismatched-column-row",
        parentInstanceRef: "table-ref", processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: 42, accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil,
        cellsComplete: true, cells: [mismatchedColumnCell])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [mismatchedColumnRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "a cell link cannot contradict the actual linked AXColumn index")

    let columnWithoutIndex = AXTableColumnContext(instanceRef: "column-ref",
        parentInstanceRef: "table-ref", processID: 42, role: "AXColumn", axIndex: nil,
        frame: column.frame, headerInstanceRef: "header-ref", headerProcessID: 42, header: "Name")
    guard let missingColumnIndexProjection = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow], visibleRowsComplete: true,
        columns: [columnWithoutIndex], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: linked cell and column refs should survive a missing column index")
    }
    let missingColumnIndexRow = missingColumnIndexProjection.visible_rows[0]
    try requireTest(missingColumnIndexRow.instance_ref == "row-ref-1"
                    && missingColumnIndexRow.cells[0].instance_ref == "cell-ref-1"
                    && missingColumnIndexRow.cells[0].column_instance_ref == "column-ref"
                    && missingColumnIndexProjection.columns[0].ax_index == nil
                    && missingColumnIndexRow.cells[0].column_ax_index == 2,
                    "missing column AXIndex preserves linked identities and known cell index")

    let cellWithoutColumnIndex = AXTableCellContext(instanceRef: "cell-index-missing",
        parentInstanceRef: "row-index-missing", processID: 42, role: "AXCell",
        frame: firstCell.frame, rowAXIndex: 42, columnAXIndex: nil,
        columnInstanceRef: "column-ref", headerInstanceRef: "header-ref", headerProcessID: 42,
        columnHeader: "Name", value: .text("Weekly report"))
    let rowWithCellIndexMissing = AXTableRowContext(instanceRef: "row-index-missing",
        parentInstanceRef: "table-ref", processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: 42, accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil,
        cellsComplete: true, cells: [cellWithoutColumnIndex])
    guard let missingCellIndexProjection = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [rowWithCellIndexMissing], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: linked cell and column refs should survive a missing cell index")
    }
    try requireTest(missingCellIndexProjection.visible_rows[0].cells[0].instance_ref == "cell-index-missing"
                    && missingCellIndexProjection.visible_rows[0].cells[0].column_instance_ref == "column-ref"
                    && missingCellIndexProjection.visible_rows[0].cells[0].column_ax_index == nil
                    && missingCellIndexProjection.columns[0].ax_index == 2,
                    "missing cell AXColumnIndexRange preserves linked identities and known column index")

    let columnAndCellIndexesMissing = AXTableColumnContext(instanceRef: "both-indexes-missing-column",
        parentInstanceRef: "table-ref", processID: 42, role: "AXColumn", axIndex: nil,
        frame: column.frame, headerInstanceRef: nil, headerProcessID: nil, header: nil)
    let cellAndColumnIndexesMissing = AXTableCellContext(instanceRef: "both-indexes-missing-cell",
        parentInstanceRef: "both-indexes-missing-row", processID: 42, role: "AXCell",
        frame: firstCell.frame, rowAXIndex: 42, columnAXIndex: nil,
        columnInstanceRef: "both-indexes-missing-column", headerInstanceRef: nil,
        headerProcessID: nil, columnHeader: nil, value: nil)
    let rowWithBothIndexesMissing = AXTableRowContext(instanceRef: "both-indexes-missing-row",
        parentInstanceRef: "table-ref", processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: 42, accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil,
        cellsComplete: true, cells: [cellAndColumnIndexesMissing])
    guard let bothIndexesMissingProjection = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [rowWithBothIndexesMissing], visibleRowsComplete: true,
        columns: [columnAndCellIndexesMissing], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: linked refs should survive both indexes being unknown")
    }
    try requireTest(bothIndexesMissingProjection.visible_rows[0].cells[0].instance_ref == "both-indexes-missing-cell"
                    && bothIndexesMissingProjection.visible_rows[0].cells[0].column_instance_ref == "both-indexes-missing-column"
                    && bothIndexesMissingProjection.visible_rows[0].cells[0].column_ax_index == nil
                    && bothIndexesMissingProjection.columns[0].ax_index == nil,
                    "both actual AX indexes may be unknown without erasing their observed refs")

    let unknownIndexColumn = AXTableColumnContext(instanceRef: "unknown-index-column",
        parentInstanceRef: "table-ref", processID: 42, role: "AXColumn", axIndex: nil,
        frame: Rect(CGRect(x: 160, y: 20, width: 150, height: 30)),
        headerInstanceRef: nil, headerProcessID: nil, header: nil)
    guard let indexedAndUnknown = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow], visibleRowsComplete: true,
        columns: [column, unknownIndexColumn], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: unknown actual AXIndex column should remain projectable")
    }
    try requireTest(indexedAndUnknown.columns.map(\.ax_index) == [2, nil]
                    && indexedAndUnknown.columns_complete,
                    "column AXIndex comes from the observed attribute, never its array ordinal")
    for invalidAXIndex in [-1, 64] {
        let invalidIndexColumn = AXTableColumnContext(instanceRef: "column-ref",
            parentInstanceRef: "table-ref", processID: 42, role: "AXColumn", axIndex: invalidAXIndex,
            frame: column.frame, headerInstanceRef: "header-ref", headerProcessID: 42, header: "Name")
        guard let invalidIndexProjection = TableViewportProjector.project(
            table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
            visibleRows: [firstRow], visibleRowsComplete: true,
            columns: [invalidIndexColumn], columnsComplete: true
        ) else {
            throw NativeFailure("self-test failed: invalid AXIndex should not invalidate observed column structure")
        }
        try requireTest(invalidIndexProjection.columns[0].ax_index == nil
                        && invalidIndexProjection.columns_complete,
                        "out-of-range AXIndex is unknown without weakening column completeness")
    }

    let unboundCell = AXTableCellContext(instanceRef: nil, parentInstanceRef: nil,
        processID: 42, role: "AXCell", frame: firstCell.frame,
        rowAXIndex: nil, columnAXIndex: nil,
        columnInstanceRef: nil, headerInstanceRef: nil, headerProcessID: nil,
        columnHeader: "Name", value: .text("Observed without flat ref"))
    let unboundRow = AXTableRowContext(instanceRef: nil, parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: true,
        cells: [unboundCell])
    guard let unbound = TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [unboundRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) else {
        throw NativeFailure("self-test failed: unbound actual AX row context should remain readable")
    }
    try requireTest(unbound.visible_rows[0].instance_ref == nil
                    && unbound.visible_rows[0].cells[0].instance_ref == nil
                    && unbound.visible_rows[0].cells[0].parent_instance_ref == nil,
                    "missing flat references remain unknown")

    let detachedCell = AXTableCellContext(instanceRef: "detached-cell", parentInstanceRef: nil,
        processID: 42, role: "AXCell", frame: firstCell.frame,
        rowAXIndex: nil, columnAXIndex: nil,
        columnInstanceRef: nil, headerInstanceRef: nil, headerProcessID: nil,
        columnHeader: nil, value: .text("Detached"))
    let rowWithoutRef = AXTableRowContext(instanceRef: nil, parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: true, cells: [detachedCell])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [rowWithoutRef], visibleRowsComplete: true,
        columns: [], columnsComplete: true
    ) == nil, "a flat cell reference cannot exist without its flat row reference")

    let secondRow = AXTableRowContext(instanceRef: "row-ref-2", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: Rect(CGRect(x: 10, y: 50, width: 300, height: 30)),
        axIndex: nil,
        accessibilityIdentifier: "opaque-row-1", axEnabled: .enabled, selected: false, selectedSettable: false, cellsComplete: true,
        cells: [])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow, secondRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "duplicate actual row identifiers are rejected")

    let foreignRow = AXTableRowContext(instanceRef: "foreign-row", parentInstanceRef: "table-ref",
        processID: 43, role: "AXRow", frame: firstRow.frame,
        axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: false, cells: [])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [foreignRow], visibleRowsComplete: false,
        columns: [column], columnsComplete: false
    ) == nil, "cross-process row contexts are rejected")

    let offscreenRow = AXTableRowContext(instanceRef: "offscreen-row", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: Rect(CGRect(x: 10, y: 180, width: 300, height: 30)),
        axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: nil, selectedSettable: nil, cellsComplete: false, cells: [])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [offscreenRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "visible row enumeration cannot contain an off-viewport row")

    let duplicateRefRow = AXTableRowContext(instanceRef: "row-ref-1", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: Rect(CGRect(x: 10, y: 50, width: 300, height: 30)),
        axIndex: nil,
        accessibilityIdentifier: "another-row-id", axEnabled: .enabled, selected: false, selectedSettable: false, cellsComplete: true,
        cells: [])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow, duplicateRefRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "duplicate per-read row references are rejected")

    let badCell = AXTableCellContext(instanceRef: "bad-cell", parentInstanceRef: "another-row",
        processID: 42, role: "AXCell", frame: firstCell.frame,
        rowAXIndex: nil, columnAXIndex: nil,
        columnInstanceRef: "column-ref", headerInstanceRef: nil,
        headerProcessID: nil,
        columnHeader: "Name", value: .text("wrong parent"))
    let badParentRow = AXTableRowContext(instanceRef: "bad-parent-row", parentInstanceRef: "table-ref",
        processID: 42, role: "AXRow", frame: firstRow.frame,
        axIndex: nil,
        accessibilityIdentifier: nil, axEnabled: .enabled, selected: false, selectedSettable: false, cellsComplete: true, cells: [badCell])
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [badParentRow], visibleRowsComplete: true,
        columns: [column], columnsComplete: true
    ) == nil, "cell parent context must be the actual AXRow")

    let controlHeader = AXTableColumnContext(instanceRef: "column-ref", parentInstanceRef: "table-ref",
        processID: 42, role: "AXColumn", axIndex: nil, frame: column.frame,
        headerInstanceRef: nil, headerProcessID: nil, header: "Name\nStatus")
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow], visibleRowsComplete: true,
        columns: [controlHeader], columnsComplete: true
    ) == nil, "control characters in column context are rejected")

    let foreignHeader = AXTableColumnContext(instanceRef: "column-ref", parentInstanceRef: "table-ref",
        processID: 42, role: "AXColumn", axIndex: nil, frame: column.frame,
        headerInstanceRef: "foreign-header", headerProcessID: 43, header: "Name")
    try requireTest(TableViewportProjector.project(
        table: table, tableAXEnabled: .attributeUnsupported, scrollContainer: scroll, viewportFrame: viewportFrame,
        visibleRows: [firstRow], visibleRowsComplete: true,
        columns: [foreignHeader], columnsComplete: true
    ) == nil, "foreign-process header context is rejected")

    return [
        "table_viewport_actual_column_index_and_range",
        "table_viewport_column_index_rejects_merged_invalid_and_boolean",
        "table_viewport_actual_row_index_and_row_range_validation",
        "table_viewport_ax_enabled_preserves_exact_typed_observation",
        "table_viewport_rejects_row_and_cell_index_disagreement",
        "table_viewport_rejects_cell_and_column_link_index_disagreement",
        "table_viewport_preserves_linked_refs_when_column_index_is_unknown",
        "table_viewport_preserves_linked_refs_when_cell_index_is_unknown",
        "table_viewport_preserves_linked_refs_when_both_indexes_are_unknown",
        "table_viewport_uses_actual_ax_column_index_without_ordinal_fallback",
        "table_viewport_invalid_ax_column_index_remains_unknown_without_degrading",
        "table_viewport_preserves_partial_enumeration_and_unknowns",
        "table_viewport_preserves_missing_flat_references",
        "table_viewport_rejects_cell_reference_without_row_reference",
        "table_viewport_rejects_duplicate_row_identifiers",
        "table_viewport_rejects_cross_process_context",
        "table_viewport_rejects_offscreen_rows",
        "table_viewport_rejects_duplicate_instance_references",
        "table_viewport_requires_actual_cell_parent",
        "table_viewport_rejects_control_characters",
        "table_viewport_rejects_foreign_header_context"
    ]
}
