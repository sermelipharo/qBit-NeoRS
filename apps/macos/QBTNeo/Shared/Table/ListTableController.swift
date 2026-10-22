import AppKit

/// A table of value rows (`Identifiable` structs rebuilt by a model on each
/// fetch) shown in a `NativeTableView`: columns with a persisted layout,
/// sorting through `KeyPathComparator`s, selection by row id, context menu,
/// primary action, Delete and Copy. When the rows change, only rows whose
/// value changed are reconfigured; a different row count reloads the table.
@MainActor
final class ListTableController<Row: Identifiable & Equatable & Sendable>: NSObject, NSTableViewDataSource, NSTableViewDelegate
where Row.ID: Sendable {
    struct Configuration {
        var identifier: String
        var accessibilityLabel: String
        var specs: [TableColumnSpec]
        var layoutKey: String?
        /// Observed; the rows in display order.
        var rows: @MainActor @Sendable () -> [Row]
        /// Observed; the selected row ids.
        var selection: @MainActor @Sendable () -> Set<Row.ID>
        var setSelection: @MainActor (Set<Row.ID>) -> Void
        /// Observed; the current sort, mapped to columns with `comparator`.
        var sortOrder: @MainActor @Sendable () -> [KeyPathComparator<Row>] = { [] }
        var setSortOrder: (@MainActor ([KeyPathComparator<Row>]) -> Void)?
        /// The comparator of a sortable column.
        var comparator: (String, SortOrder) -> KeyPathComparator<Row>? = { _, _ in nil }
        /// Text cell content; the default cell kind.
        var content: @MainActor (String, Row) -> TableCellContent
        /// Cells that are not text cells (returns nil for text columns).
        var makeCell: (@MainActor (NSTableView, String, Row) -> NSView?)?
        var configureCell: (@MainActor (NSView, String, Row) -> Void)?
        /// Context menu for the targeted row ids (empty: click below the rows).
        var menu: (@MainActor (Set<Row.ID>) -> NSMenu?)?
        var primaryAction: (@MainActor (Set<Row.ID>) -> Void)?
        var deleteAction: (@MainActor () -> Void)?
        var copyAction: (@MainActor () -> Void)?
        var pasteAction: (@MainActor () -> Void)?
        var allowsMultipleSelection = true
        /// False for single-column lists (the Execution Log).
        var showsHeader = true
        /// Whether the "Alternating row colors" setting applies (not to article lists).
        var followsAlternatingRowsSetting = true
        /// Variable row heights (wrapped text): the height of a row for a column width.
        var rowHeight: (@MainActor (Row, CGFloat) -> CGFloat)?
    }

    let table = NativeTableView()
    let scrollView = NSScrollView()
    private let configuration: Configuration
    private var columns: TableColumnsController?
    private let observations = ObservationTasks()
    private(set) var rows: [Row] = []
    private var isApplyingSelection = false
    private var isApplyingSort = false
    /// Row heights by id, valid for `heightWidth`.
    private var heights: [Row.ID: CGFloat] = [:]
    private var heightWidth: CGFloat = 0
    private static var textID: NSUserInterfaceItemIdentifier { NSUserInterfaceItemIdentifier("list.text") }

    init(_ configuration: Configuration) {
        self.configuration = configuration
        super.init()
        table.identifier = NSUserInterfaceItemIdentifier(configuration.identifier)
        table.setAccessibilityLabel(configuration.accessibilityLabel)
        table.style = .inset
        table.allowsMultipleSelection = configuration.allowsMultipleSelection
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        if let menu = configuration.menu {
            table.menuProvider = { [weak self] indexes in
                guard let self else { return nil }
                return menu(Set(ids(indexes)))
            }
        }
        if let action = configuration.primaryAction {
            table.primaryAction = { [weak self] indexes in
                guard let self else { return }
                action(Set(ids(indexes)))
            }
        }
        if let action = configuration.deleteAction { table.deleteAction = { _ in action() } }
        table.copyAction = configuration.copyAction
        table.pasteAction = configuration.pasteAction
        columns = TableColumnsController(table: table, specs: configuration.specs, storageKey: configuration.layoutKey)
        if !configuration.showsHeader {
            table.headerView = nil
            table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        }
        if configuration.rowHeight != nil {
            NotificationCenter.default.addObserver(self, selector: #selector(columnDidResize),
                                                   name: NSTableView.columnDidResizeNotification, object: table)
        }
        scrollView.documentView = table
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder

        rows = configuration.rows()
        table.dataSource = self
        table.delegate = self
        table.reloadData()
        applySortIndicators(configuration.sortOrder())
        applySelection(configuration.selection())
    }

    func start(gui: GUIPreferences) {
        let configuration = configuration
        observations.observe(configuration.rows, apply: { [weak self] in self?.applyRows($0) })
        observations.observe(configuration.selection, apply: { [weak self] in self?.applySelection($0) })
        observations.observe(configuration.sortOrder, apply: { [weak self] in self?.applySortIndicators($0) })
        if configuration.followsAlternatingRowsSetting {
            observations.observe({ gui.alternatingRowColors }, apply: { [weak self] in self?.table.wantsAlternatingRows = $0 })
        }
    }

    /// Runs `apply` (typically `refreshAll`) whenever `value` changes.
    func observe<Value: Sendable>(_ value: @escaping @MainActor @Sendable () -> Value, apply: @escaping @MainActor (Value) -> Void) {
        observations.observe(value, apply: apply)
    }

    func stop() {
        observations.cancelAll()
        columns?.flush()
    }

    func setColumn(_ id: String, available: Bool) {
        columns?.setAvailable(available, column: id)
    }

    // MARK: Rows

    private func applyRows(_ next: [Row]) {
        guard next != rows else { return }
        let previous = rows
        rows = next
        if heights.count > 2 * next.count + 64 {
            let kept = Set(next.map(\.id))
            heights = heights.filter { kept.contains($0.key) }
        }
        if previous.count != next.count {
            // Row indexes are stale until the model's selection is applied again below.
            isApplyingSelection = true
            table.reloadData()
            isApplyingSelection = false
        } else {
            var changed = IndexSet()
            for (index, pair) in zip(next, previous).enumerated() where pair.0 != pair.1 { changed.insert(index) }
            refresh(rows: changed)
        }
        applySelection(configuration.selection())
    }

    /// Reconfigures every realized cell (after a display setting changed).
    func refreshAll() {
        refresh(rows: IndexSet(integersIn: 0 ..< rows.count))
    }

    private func refresh(rows indexes: IndexSet) {
        guard !indexes.isEmpty else { return }
        let columns = table.tableColumns
        table.enumerateAvailableRowViews { rowView, index in
            guard indexes.contains(index), index < rows.count else { return }
            for (position, column) in columns.enumerated() where !column.isHidden {
                guard let view = rowView.view(atColumn: position) as? NSView else { continue }
                configure(view, column: column.identifier.rawValue, row: rows[index])
            }
        }
    }

    func ids(_ indexes: IndexSet) -> [Row.ID] {
        indexes.compactMap { $0 < rows.count ? rows[$0].id : nil }
    }

    // MARK: Cells

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let tableColumn, row < rows.count else { return nil }
        let id = tableColumn.identifier.rawValue
        if let custom = configuration.makeCell?(tableView, id, rows[row]) { return custom }
        let cell = TableTextCell.make(in: tableView, identifier: Self.textID)
        cell.configure(configuration.content(id, rows[row]))
        return cell
    }

    private func configure(_ view: NSView, column: String, row: Row) {
        if let cell = view as? TableTextCell, cell.identifier == Self.textID {
            cell.configure(configuration.content(column, row))
        } else {
            configuration.configureCell?(view, column, row)
        }
    }

    // MARK: Row heights

    override func responds(to aSelector: Selector!) -> Bool {
        if aSelector == #selector(tableView(_:heightOfRow:)) {
            return MainActor.assumeIsolated { configuration.rowHeight != nil }
        }
        return super.responds(to: aSelector)
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        guard let rowHeight = configuration.rowHeight, row < rows.count else { return tableView.rowHeight }
        let width = tableView.tableColumns.first { !$0.isHidden }?.width ?? tableView.bounds.width
        if width != heightWidth {
            heights.removeAll(keepingCapacity: true)
            heightWidth = width
        }
        let id = rows[row].id
        if let height = heights[id] { return height }
        let height = rowHeight(rows[row], width)
        heights[id] = height
        return height
    }

    @objc private func columnDidResize(_ notification: Notification) {
        guard !rows.isEmpty else { return }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0
            table.noteHeightOfRows(withIndexesChanged: IndexSet(integersIn: 0 ..< rows.count))
        }
    }

    // MARK: Selection and sorting

    private func applySelection(_ selection: Set<Row.ID>) {
        var indexes = IndexSet()
        if !selection.isEmpty {
            for (index, row) in rows.enumerated() where selection.contains(row.id) { indexes.insert(index) }
        }
        guard indexes != table.selectedRowIndexes else { return }
        isApplyingSelection = true
        table.selectRowIndexes(indexes, byExtendingSelection: false)
        isApplyingSelection = false
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isApplyingSelection else { return }
        let selection = Set(ids(table.selectedRowIndexes))
        if selection != configuration.selection() { configuration.setSelection(selection) }
    }

    private func applySortIndicators(_ order: [KeyPathComparator<Row>]) {
        let keys = configuration.specs.filter(\.isSortable).map(\.id)
        let descriptors = order.compactMap { comparator -> NSSortDescriptor? in
            let key = keys.first { configuration.comparator($0, .forward)?.keyPath == comparator.keyPath }
            return key.map { NSSortDescriptor(key: $0, ascending: comparator.order == .forward) }
        }
        guard descriptors != table.sortDescriptors else { return }
        isApplyingSort = true
        table.sortDescriptors = descriptors
        isApplyingSort = false
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard !isApplyingSort, let setSortOrder = configuration.setSortOrder else { return }
        setSortOrder(tableView.sortDescriptors.compactMap { descriptor in
            descriptor.key.flatMap { configuration.comparator($0, descriptor.ascending ? .forward : .reverse) }
        })
    }
}
