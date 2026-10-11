import Foundation

/// A bounded, per-read context for one real AXTable and its current scroll
/// viewport. `visible_rows_complete` describes only the AXVisibleRows read; it
/// never changes the completeness of the surrounding structure traversal.
struct TableViewport: Encodable, Sendable, Equatable {
    let table_instance_ref: String
    let scroll_container_instance_ref: String
    let process_id: UInt32
    let table_frame: Rect
    let viewport_frame: Rect
    let ax_enabled: AxEnabledObservation
    let visible_rows_complete: Bool
    let visible_rows: [TableViewportRow]
    let columns_complete: Bool
    let columns: [TableViewportColumn]
}

struct TableViewportRow: Encodable, Sendable, Equatable {
    let instance_ref: String?
    let parent_instance_ref: String
    let frame: Rect
    /// The actual AXIndex attribute. Missing or invalid remains unknown.
    let ax_index: UInt32?
    /// The actual AXIdentifier attribute. Missing remains unknown.
    let accessibility_identifier: String?
    let ax_enabled: AxEnabledObservation
    /// The actual AXSelected attribute. Missing remains unknown.
    let selected: Bool?
    let ax_selected_settable: Bool?
    let cells_complete: Bool
    let cells: [TableViewportCell]
}

struct TableViewportColumn: Encodable, Sendable, Equatable {
    let instance_ref: String?
    let parent_instance_ref: String
    let frame: Rect
    /// The actual AXIndex attribute. Missing or invalid stays unknown.
    let ax_index: Int?
    let header_instance_ref: String?
    let header: String?
}

struct TableViewportCell: Encodable, Sendable, Equatable {
    let instance_ref: String?
    let parent_instance_ref: String?
    let frame: Rect
    /// The actual singleton AXRowIndexRange location. Merged/invalid stays unknown.
    let row_ax_index: UInt32?
    /// The actual singleton AXColumnIndexRange location. Invalid stays unknown.
    let column_ax_index: UInt32?
    let column_instance_ref: String?
    let header_instance_ref: String?
    let column_header: String?
    let value: UiValue?
}

/// Value-only contexts collected from actual AX nodes by the AX reader.
/// These are intentionally separate from task/evaluator data and do not
/// manufacture rows, selection state, or column relationships.
struct AXTableNodeContext: Sendable {
    let instanceRef: String
    let parentInstanceRef: String?
    let processID: UInt32
    let role: String
    let frame: Rect
}

struct AXTableCellContext: Sendable {
    let instanceRef: String?
    let parentInstanceRef: String?
    let processID: UInt32
    let role: String
    let frame: Rect
    let rowAXIndex: UInt32?
    let columnAXIndex: UInt32?
    let columnInstanceRef: String?
    let headerInstanceRef: String?
    let headerProcessID: UInt32?
    let columnHeader: String?
    let value: UiValue?
}

struct AXTableRowContext: Sendable {
    let instanceRef: String?
    let parentInstanceRef: String
    let processID: UInt32
    let role: String
    let frame: Rect
    let axIndex: UInt32?
    let accessibilityIdentifier: String?
    let axEnabled: AxEnabledObservation
    let selected: Bool?
    let selectedSettable: Bool?
    let cellsComplete: Bool
    let cells: [AXTableCellContext]
}

struct AXTableColumnContext: Sendable {
    let instanceRef: String?
    let parentInstanceRef: String
    let processID: UInt32
    let role: String
    let axIndex: Int?
    let frame: Rect
    let headerInstanceRef: String?
    let headerProcessID: UInt32?
    let header: String?
}

/// Projects only the real AXVisibleRows enumeration and preserves unknown
/// AX attributes. The caller must set the completeness flags from the actual
/// bounded AX child/attribute reads.
enum TableViewportProjector {
    private static let maximumReferenceBytes = 256
    private static let maximumTextBytes = 2_048
    private static let maximumRows = 256
    private static let maximumColumns = 64
    private static let maximumCellsPerRow = 128
    private static let maximumCells = 2_048

    static func project(
        table: AXTableNodeContext,
        tableAXEnabled: AxEnabledObservation,
        scrollContainer: AXTableNodeContext,
        viewportFrame: Rect,
        visibleRows: [AXTableRowContext],
        visibleRowsComplete: Bool,
        columns: [AXTableColumnContext],
        columnsComplete: Bool
    ) -> TableViewport? {
        guard table.role == "AXTable",
              scrollContainer.role == "AXScrollArea",
              table.instanceRef != scrollContainer.instanceRef,
              table.parentInstanceRef == scrollContainer.instanceRef,
              table.processID > 0,
              table.processID <= UInt32(Int32.max),
              scrollContainer.processID == table.processID,
              visibleRows.count <= maximumRows,
              columns.count <= maximumColumns,
              validReference(table.instanceRef),
              validReference(scrollContainer.instanceRef),
              validFrame(table.frame),
              validFrame(scrollContainer.frame),
              validFrame(viewportFrame),
              contains(scrollContainer.frame, viewportFrame),
              table.frame.intersects(viewportFrame) else {
            return nil
        }

        var references: Set<String> = [table.instanceRef, scrollContainer.instanceRef]
        var rowIdentifiers: Set<String> = []
        var totalCells = 0
        var projectedRows: [TableViewportRow] = []
        projectedRows.reserveCapacity(visibleRows.count)

        for row in visibleRows {
            guard row.role == "AXRow",
                  row.parentInstanceRef == table.instanceRef,
                  row.processID == table.processID,
                  optionalReferenceIsValid(row.instanceRef),
                  row.instanceRef.map({ references.insert($0).inserted }) ?? true,
                  validFrame(row.frame),
                  contains(table.frame, row.frame),
                  row.frame.intersects(viewportFrame),
                  row.cells.count <= maximumCellsPerRow else {
                return nil
            }
            if let identifier = row.accessibilityIdentifier {
                guard validText(identifier, maximumBytes: maximumReferenceBytes),
                      rowIdentifiers.insert(identifier).inserted else {
                    return nil
                }
            }
            totalCells += row.cells.count
            guard totalCells <= maximumCells else { return nil }

            // AXRowIndexRange on each cell describes its owning row. Keep the
            // separately read AXRow AXIndex as the row fact; never fill one
            // from the other, and reject contradictory known observations.
            var observedRowIndex = row.axIndex
            for cell in row.cells {
                if let cellRowIndex = cell.rowAXIndex {
                    if let observedRowIndex, observedRowIndex != cellRowIndex {
                        return nil
                    }
                    observedRowIndex = cellRowIndex
                }
            }

            var projectedCells: [TableViewportCell] = []
            projectedCells.reserveCapacity(row.cells.count)
            for cell in row.cells {
                guard cell.role == "AXCell",
                      cell.instanceRef == nil || row.instanceRef != nil,
                      cell.parentInstanceRef == row.instanceRef,
                      cell.processID == table.processID,
                      optionalReferenceIsValid(cell.instanceRef),
                      cell.instanceRef.map({ references.insert($0).inserted }) ?? true,
                      validFrame(cell.frame),
                      contains(row.frame, cell.frame),
                      cell.columnAXIndex.map({ $0 < UInt32(maximumColumns) }) ?? true,
                      optionalReferenceIsValid(cell.columnInstanceRef),
                      optionalReferenceIsValid(cell.headerInstanceRef),
                      relatedProcessIsValid(cell.headerInstanceRef, processID: cell.headerProcessID,
                                            expectedProcessID: table.processID),
                      optionalTextIsValid(cell.columnHeader, maximumBytes: maximumTextBytes),
                      optionalValueIsValid(cell.value) else {
                    return nil
                }
                projectedCells.append(TableViewportCell(
                    instance_ref: cell.instanceRef,
                    parent_instance_ref: cell.parentInstanceRef,
                    frame: cell.frame,
                    row_ax_index: cell.rowAXIndex,
                    column_ax_index: cell.columnAXIndex,
                    column_instance_ref: cell.columnInstanceRef,
                    header_instance_ref: cell.headerInstanceRef,
                    column_header: cell.columnHeader,
                    value: cell.value
                ))
            }
            projectedRows.append(TableViewportRow(
                instance_ref: row.instanceRef,
                parent_instance_ref: row.parentInstanceRef,
                frame: row.frame,
                ax_index: row.axIndex,
                accessibility_identifier: row.accessibilityIdentifier,
                ax_enabled: row.axEnabled,
                selected: row.selected,
                ax_selected_settable: row.selectedSettable,
                cells_complete: row.cellsComplete,
                cells: projectedCells
            ))
        }

        var projectedColumns: [TableViewportColumn] = []
        projectedColumns.reserveCapacity(columns.count)
        var columnReferences: Set<String> = []
        for column in columns {
            let actualAXIndex = column.axIndex.flatMap { (0..<maximumColumns).contains($0) ? $0 : nil }
            guard column.role == "AXColumn",
                  column.processID == table.processID,
                  column.parentInstanceRef == table.instanceRef
                    || column.parentInstanceRef == scrollContainer.instanceRef,
                  optionalReferenceIsValid(column.instanceRef),
                  column.instanceRef.map({ references.insert($0).inserted }) ?? true,
                  column.instanceRef.map({ columnReferences.insert($0).inserted }) ?? true,
                  validFrame(column.frame),
                  column.frame.intersects(table.frame)
                    || column.frame.intersects(scrollContainer.frame),
                  optionalReferenceIsValid(column.headerInstanceRef),
                  relatedProcessIsValid(column.headerInstanceRef, processID: column.headerProcessID,
                                        expectedProcessID: table.processID),
                  optionalTextIsValid(column.header, maximumBytes: maximumTextBytes) else {
                return nil
            }
            projectedColumns.append(TableViewportColumn(
                instance_ref: column.instanceRef,
                parent_instance_ref: column.parentInstanceRef,
                frame: column.frame,
                ax_index: actualAXIndex,
                header_instance_ref: column.headerInstanceRef,
                header: column.header
            ))
        }

        for row in projectedRows {
            for cell in row.cells {
                if let columnRef = cell.column_instance_ref,
                   let column = projectedColumns.first(where: { $0.instance_ref == columnRef }) {
                    guard columnReferences.contains(columnRef) else {
                        return nil
                    }
                    if let columnIndex = column.ax_index,
                       let cellIndex = cell.column_ax_index,
                       UInt32(exactly: columnIndex) != cellIndex {
                        return nil
                    }
                } else if cell.column_instance_ref != nil {
                    return nil
                }
            }
        }

        return TableViewport(
            table_instance_ref: table.instanceRef,
            scroll_container_instance_ref: scrollContainer.instanceRef,
            process_id: table.processID,
            table_frame: table.frame,
            viewport_frame: viewportFrame,
            ax_enabled: tableAXEnabled,
            visible_rows_complete: visibleRowsComplete,
            visible_rows: projectedRows,
            columns_complete: columnsComplete,
            columns: projectedColumns
        )
    }

    private static func validReference(_ value: String) -> Bool {
        validText(value, maximumBytes: maximumReferenceBytes)
    }

    private static func optionalReferenceIsValid(_ value: String?) -> Bool {
        value.map(validReference) ?? true
    }

    private static func relatedProcessIsValid(_ reference: String?, processID: UInt32?,
                                              expectedProcessID: UInt32) -> Bool {
        switch (reference, processID) {
        case (nil, nil): true
        case (.some, .some(let actual)): actual == expectedProcessID
        default: false
        }
    }

    private static func optionalTextIsValid(_ value: String?, maximumBytes: Int) -> Bool {
        value.map { validText($0, maximumBytes: maximumBytes, allowEmpty: true) } ?? true
    }

    private static func validText(_ value: String, maximumBytes: Int, allowEmpty: Bool = false) -> Bool {
        (allowEmpty || !value.isEmpty) && value.utf8.count <= maximumBytes
            && !value.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
    }

    private static func validFrame(_ frame: Rect) -> Bool {
        frame.valid && frame.width > 0 && frame.height > 0
            && [frame.x, frame.y, frame.width, frame.height].allSatisfy { abs($0) <= 100_000 }
    }

    private static func contains(_ outer: Rect, _ inner: Rect) -> Bool {
        inner.x >= outer.x && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    }

    private static func optionalValueIsValid(_ value: UiValue?) -> Bool {
        guard let value else { return true }
        switch value {
        case .bool(_):
            return true
        case .number(let number):
            return number.isFinite
        case .text(let text):
            return validText(text, maximumBytes: maximumTextBytes, allowEmpty: true)
        }
    }
}
