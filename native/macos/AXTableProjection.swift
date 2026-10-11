import Foundation
import ApplicationServices

/// A button is eligible only when it is the actual AXHeader reference returned
/// by the observed column. It cannot broaden the intermediate parent chain.
func scopedTableHeaderRoleAllowed(_ role: String, isHeaderLeaf: Bool) -> Bool {
    ["AXHeader", kAXStaticTextRole, kAXGroupRole].contains(role)
        || (isHeaderLeaf && role == kAXButtonRole)
}

func observedTableColumnIndex(_ raw: CFTypeRef?, cell: Bool) -> Int? {
    if cell {
        guard let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
        var range = CFRange()
        guard AXValueGetValue(unsafeDowncast(raw, to: AXValue.self), .cfRange, &range),
              range.length == 1, (0..<64).contains(range.location) else { return nil }
        return range.location
    }
    guard let number = raw as? NSNumber, CFGetTypeID(number) == CFNumberGetTypeID(),
          number.doubleValue.isFinite,
          number.doubleValue.rounded(.towardZero) == number.doubleValue,
          number.doubleValue >= 0, number.doubleValue < 64 else { return nil }
    return number.intValue
}

func observedTableRowIndex(_ raw: CFTypeRef?) -> UInt32? {
    guard let raw, CFGetTypeID(raw) == CFNumberGetTypeID(),
          let number = raw as? NSNumber else { return nil }
    let value = number.doubleValue
    guard value.isFinite, value.rounded(.towardZero) == value,
          value >= 0, value <= Double(UInt32.max) else { return nil }
    return UInt32(value)
}

func observedTableRowIndexRange(_ raw: CFTypeRef?) -> UInt32? {
    guard let raw, CFGetTypeID(raw) == AXValueGetTypeID() else { return nil }
    var range = CFRange()
    guard AXValueGetValue(unsafeDowncast(raw, to: AXValue.self), .cfRange, &range),
          range.length == 1, let index = UInt32(exactly: range.location) else { return nil }
    return index
}

func classifyTableAXEnabled(error: AXError, value: CFTypeRef?) -> AxEnabledObservation {
    if error == .attributeUnsupported { return .attributeUnsupported }
    guard error == .success, let value,
          CFGetTypeID(value) == CFBooleanGetTypeID(),
          let number = value as? NSNumber else {
        return .unavailable
    }
    return number.boolValue ? .enabled : .disabled
}

// AX references stay on the existing synchronous AX worker. Per-read object
// IDs are resolved only after traversal; unobserved rows never gain a binding.
struct ObservedAXTable {
    struct Cell {
        let element: AXUIElement
        let frame: Rect
        let value: UiValue?
        let rowIndex: UInt32?
        let columnIndex: Int?
    }
    struct Row {
        let element: AXUIElement
        let frame: Rect
        let index: UInt32?
        let identifier: String?
        let axEnabled: AxEnabledObservation
        let selected: Bool?
        let selectedSettable: Bool?
        let complete: Bool
        let cells: [Cell]
    }
    struct Column {
        let element: AXUIElement
        let frame: Rect
        let header: AXUIElement?
        let title: String?
        /// The validated value of this AXColumn's AXIndex attribute, not its
        /// position in the returned AXColumns array.
        let index: Int?
    }
    let tableID: String
    let scrollID: String
    let tableFrame: Rect
    let scrollFrame: Rect
    let viewport: Rect
    let axEnabled: AxEnabledObservation
    let rowsComplete: Bool
    let rows: [Row]
    let columnsComplete: Bool
    let columns: [Column]
    /// When set, only these scoped leaf objects were materialized into the
    /// ordinary source tree. `nil` keeps the normal full-tree projection path.
    var scopedMaterializedRefs: Set<String>? = nil
}

extension AXReader {
    private func readTableAXEnabled(_ element: AXUIElement) -> AxEnabledObservation {
        guard budget(), beforeQuery(element) else { return .unavailable }
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, kAXEnabledAttribute as CFString, &value)
        guard budget() else { return .unavailable }
        return classifyTableAXEnabled(error: error, value: value)
    }

    private func tableColumnIndex(_ element: AXUIElement, cell: Bool) -> Int? {
        observedTableColumnIndex(attribute(element, cell ? kAXColumnIndexRangeAttribute : kAXIndexAttribute), cell: cell)
    }
    private func tableOwned(_ element: AXUIElement) -> Bool {
        var pid: pid_t = 0
        guard budget(), beforeQuery(element) else { return false }
        let error = AXUIElementGetPid(element, &pid)
        let timely = request.background_target == nil ? (error != .success || budget()) : budget()
        return error == .success && timely && pid > 0 && UInt32(pid) == app.pid
    }

    private func tableElement(_ element: AXUIElement, attribute name: String) -> AXUIElement? {
        guard let raw = attribute(element, name), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        let value = unsafeDowncast(raw, to: AXUIElement.self)
        return tableOwned(value) ? value : nil
    }

    // Local coverage is established from the actual AX enumeration result,
    // independently of an already partial flat-tree traversal.
    private func tableChildren(_ element: AXUIElement, name: String, limit: Int) -> ([AXUIElement], Bool) {
        guard tableOwned(element), budget(), beforeQuery(element) else { return ([], false) }
        var count = 0
        let countError = AXUIElementGetAttributeValueCount(element, name as CFString, &count)
        let countTimely = request.background_target == nil
            ? (countError != .success || budget()) : budget()
        guard countError == .success, countTimely, count >= 0, count <= limit else { return ([], false) }
        if count == 0 { return ([], true) }
        guard beforeQuery(element) else { return ([], false) }
        var raw: CFArray?
        let copyError = AXUIElementCopyAttributeValues(element, name as CFString, 0, count, &raw)
        let copyTimely = request.background_target == nil
            ? (copyError != .success || budget()) : budget()
        guard copyError == .success, copyTimely, let values = raw as? [AXUIElement], values.count == count,
              values.allSatisfy({ tableOwned($0) }) else { return ([], false) }
        return (values, true)
    }

    private func tableValue(_ element: AXUIElement) -> UiValue? {
        guard tableOwned(element), let role = string(element, kAXRoleAttribute),
              !isSensitiveField(role: role, subrole: string(element, kAXSubroleAttribute)) else { return nil }
        if let raw = attribute(element, kAXValueAttribute) {
            if let number = raw as? NSNumber { return axNumericValue(number, role: role) }
            if let text = raw as? String, text.utf8.count <= 2048 { return .text(text) }
        }
        guard role == kAXCellRole else { return nil }
        let (children, complete) = tableChildren(element, name: kAXChildrenAttribute, limit: 8)
        guard complete, children.count == 1, let child = children.first,
              string(child, kAXRoleAttribute) == kAXStaticTextRole,
              let parent = tableElement(child, attribute: kAXParentAttribute), CFEqual(parent, element) else { return nil }
        return tableValue(child)
    }

    func observeTableViewport(_ table: AXUIElement, tableID: String, parentID: String?,
                              viewport: Rect) -> ObservedAXTable? {
        guard observedTables.count < 32, let parentID,
              let parentObject = objects.first(where: { $0.instance_ref == parentID }),
              parentObject.role == kAXScrollAreaRole,
              let parent = tableElement(table, attribute: kAXParentAttribute),
              observedElements.contains(where: { $0.id == parentID && CFEqual($0.element, parent) }),
              let tableFrame = frame(table), let scrollFrame = frame(parent) else { return nil }
        let clipped = Rect(scrollFrame.cgRect.intersection(viewport.cgRect))
        guard clipped.valid, clipped.width > 0, clipped.height > 0 else { return nil }
        let tableAXEnabled = readTableAXEnabled(table)
        let (rowElements, rowsRead) = tableChildren(table, name: kAXVisibleRowsAttribute, limit: 256)
        var rowsComplete = rowsRead
        var rows: [ObservedAXTable.Row] = []
        for row in rowElements {
            guard string(row, kAXRoleAttribute) == kAXRowRole,
                  let actualParent = tableElement(row, attribute: kAXParentAttribute), CFEqual(actualParent, table),
                  let rowFrame = frame(row) else { rowsComplete = false; continue }
            let identifier = attribute(row, kAXIdentifierAttribute) as? String
            let rowAXEnabled = readTableAXEnabled(row)
            let rawSelected = attribute(row, kAXSelectedAttribute)
            let selected = rawSelected.flatMap { CFGetTypeID($0) == CFBooleanGetTypeID() ? ($0 as? NSNumber)?.boolValue : nil }
            var settable = DarwinBoolean(false)
            let selectedSettable: Bool?
            if budget(), beforeQuery(row) {
                let selectedError = AXUIElementIsAttributeSettable(row, kAXSelectedAttribute as CFString, &settable)
                let timely = request.background_target == nil
                    ? (selectedError != .success || budget()) : budget()
                selectedSettable = selectedError == .success && timely ? settable.boolValue : nil
            } else {
                selectedSettable = nil
            }
            let (cellElements, cellsRead) = tableChildren(row, name: kAXChildrenAttribute, limit: 128)
            var cellsComplete = cellsRead
            var cells: [ObservedAXTable.Cell] = []
            for cell in cellElements {
                guard string(cell, kAXRoleAttribute) == kAXCellRole,
                      let actualParent = tableElement(cell, attribute: kAXParentAttribute), CFEqual(actualParent, row),
                      let cellFrame = frame(cell) else { cellsComplete = false; continue }
                cells.append(.init(element: cell, frame: cellFrame, value: tableValue(cell),
                    rowIndex: observedTableRowIndexRange(attribute(cell, kAXRowIndexRangeAttribute)),
                    columnIndex: tableColumnIndex(cell, cell: true)))
            }
            rows.append(.init(element: row, frame: rowFrame,
                index: observedTableRowIndex(attribute(row, kAXIndexAttribute)), identifier: identifier,
                axEnabled: rowAXEnabled, selected: selected, selectedSettable: selectedSettable,
                complete: cellsComplete && budget(), cells: cells))
        }
        let (columnElements, columnsRead) = tableChildren(table, name: kAXColumnsAttribute, limit: 64)
        var columnsComplete = columnsRead
        var columns: [ObservedAXTable.Column] = []
        for column in columnElements {
            guard string(column, kAXRoleAttribute) == kAXColumnRole,
                  let actualParent = tableElement(column, attribute: kAXParentAttribute), CFEqual(actualParent, table),
                  let columnFrame = frame(column) else { columnsComplete = false; continue }
            let header = tableElement(column, attribute: kAXHeaderAttribute)
            columns.append(.init(element: column, frame: columnFrame, header: header,
                title: header.flatMap { string($0, kAXTitleAttribute) },
                index: tableColumnIndex(column, cell: false)))
        }
        return .init(tableID: tableID, scrollID: parentID, tableFrame: tableFrame, scrollFrame: scrollFrame,
            viewport: clipped, axEnabled: tableAXEnabled,
            rowsComplete: rowsComplete && budget(), rows: rows,
            columnsComplete: columnsComplete && budget(), columns: columns)
    }

    func projectTableViewport(_ table: ObservedAXTable) -> TableViewport? {
        func ref(_ element: AXUIElement) -> String? {
            guard let observed = observedElements.first(where: { CFEqual($0.element, element) }) else {
                return nil
            }
            if let scopedMaterializedRefs = table.scopedMaterializedRefs,
               !scopedMaterializedRefs.contains(observed.id),
               observed.id != table.tableID, observed.id != table.scrollID {
                return nil
            }
            return observed.id
        }
        return TableViewportProjector.project(
            table: .init(instanceRef: table.tableID, parentInstanceRef: table.scrollID,
                processID: app.pid, role: kAXTableRole, frame: table.tableFrame),
            tableAXEnabled: table.axEnabled,
            scrollContainer: .init(instanceRef: table.scrollID, parentInstanceRef: nil,
                processID: app.pid, role: kAXScrollAreaRole, frame: table.scrollFrame),
            viewportFrame: table.viewport,
            visibleRows: table.rows.map { row in
                let rowRef = ref(row.element)
                return .init(instanceRef: rowRef, parentInstanceRef: table.tableID, processID: app.pid,
                    role: kAXRowRole, frame: row.frame, axIndex: row.index,
                    accessibilityIdentifier: row.identifier,
                    axEnabled: row.axEnabled,
                    selected: row.selected, selectedSettable: row.selectedSettable, cellsComplete: row.complete,
                    cells: row.cells.map { cell in
                        let matches = table.columns.filter { cell.columnIndex != nil && $0.index == cell.columnIndex }
                        let column = table.columnsComplete && matches.count == 1 ? matches.first : nil
                        let headerRef = column?.header.flatMap(ref)
                        return .init(instanceRef: ref(cell.element), parentInstanceRef: rowRef,
                            processID: app.pid, role: kAXCellRole, frame: cell.frame,
                            rowAXIndex: cell.rowIndex,
                            columnAXIndex: cell.columnIndex.flatMap(UInt32.init(exactly:)),
                            columnInstanceRef: column.flatMap { ref($0.element) },
                            headerInstanceRef: headerRef, headerProcessID: headerRef == nil ? nil : app.pid,
                            columnHeader: column?.title, value: cell.value)
                    })
            }, visibleRowsComplete: table.rowsComplete,
            columns: table.columns.map { column in
                let headerRef = column.header.flatMap(ref)
                return .init(instanceRef: ref(column.element), parentInstanceRef: table.tableID,
                    processID: app.pid, role: kAXColumnRole, axIndex: column.index, frame: column.frame,
                    headerInstanceRef: headerRef, headerProcessID: headerRef == nil ? nil : app.pid,
                    header: column.title)
            }, columnsComplete: table.columnsComplete)
    }
}
