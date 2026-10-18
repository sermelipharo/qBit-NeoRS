import AppKit

/// A torrent content tree (`FileTreeModel`, flattened rows) in a
/// `NativeTableView`: disclosure chevrons and checkboxes in the name
/// column, priority pull-downs, progress bars, sorting, Space to toggle
/// the download checkboxes, Right/Left arrow to expand/collapse folders.
/// Used by the Content tab and the Add Torrent dialog.
@MainActor
final class FileTreeTableController: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    struct Configuration {
        var identifier: String
        var columns: [FileColumn]
        var layoutKey: String?
        var nameMinWidth: CGFloat = 160
        var nameIdealWidth: CGFloat = 360
        /// Called with the file priorities changed by a checkbox, a priority
        /// pull-down, Space or the context menu.
        var apply: @MainActor ([FilePriority: [Int64]]) -> Void
        var menu: (@MainActor ([FileNode]) -> NSMenu?)?
        var primaryAction: (@MainActor ([FileNode]) -> Void)?
    }

    let table = NativeTableView()
    let scrollView = NSScrollView()
    private let tree: FileTreeModel
    private let configuration: Configuration
    private var columns: TableColumnsController?
    private let observations = ObservationTasks()
    private var rows: [FileNode] = []
    private var isApplyingSelection = false
    private var isApplyingSort = false
    private static let textID = NSUserInterfaceItemIdentifier("file.text")
    private static let progressID = NSUserInterfaceItemIdentifier("file.progress")

    init(tree: FileTreeModel, configuration: Configuration) {
        self.tree = tree
        self.configuration = configuration
        super.init()
        table.identifier = NSUserInterfaceItemIdentifier(configuration.identifier)
        table.setAccessibilityLabel(String(localized: "Content"))
        table.style = .inset
        table.allowsMultipleSelection = true
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        table.menuProvider = { [weak self] indexes in
            guard let self, let menu = configuration.menu else { return nil }
            return menu(nodes(indexes))
        }
        table.primaryAction = { [weak self] indexes in
            guard let self else { return }
            configuration.primaryAction?(nodes(indexes))
        }
        table.keyHandler = { [weak self] event in self?.handleKey(event) ?? false }
        columns = TableColumnsController(table: table, specs: configuration.columns.map(spec), storageKey: configuration.layoutKey)
        scrollView.documentView = table
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder

        rows = tree.rows
        table.dataSource = self
        table.delegate = self
        table.reloadData()
        applySortIndicators(tree.sortOrder)
        applySelection(tree.selection)
    }

    private func spec(_ column: FileColumn) -> TableColumnSpec {
        switch column {
        case .name: TableColumnSpec(column.rawValue, column.title, minWidth: configuration.nameMinWidth,
                                    idealWidth: configuration.nameIdealWidth, isHideable: false)
        case .size: TableColumnSpec(column.rawValue, column.title, minWidth: 60, idealWidth: 90, alignment: .right)
        case .progress: TableColumnSpec(column.rawValue, column.title, minWidth: 60, idealWidth: 110)
        case .priority: TableColumnSpec(column.rawValue, column.title, minWidth: 80, idealWidth: 130)
        case .remaining: TableColumnSpec(column.rawValue, column.title, minWidth: 60, idealWidth: 90, alignment: .right)
        case .availability: TableColumnSpec(column.rawValue, column.title, minWidth: 60, idealWidth: 90, alignment: .right)
        }
    }

    func start(gui: GUIPreferences) {
        let tree = tree
        observations.observe({ tree.rows }, apply: { [weak self] in self?.applyRows($0) })
        observations.observe({ tree.valuesRevision }, apply: { [weak self] _ in self?.refreshAll() })
        observations.observe({ tree.selection }, apply: { [weak self] in self?.applySelection($0) })
        observations.observe({ tree.sortOrder }, apply: { [weak self] in self?.applySortIndicators($0) })
        observations.observe({ gui.alternatingRowColors }, apply: { [weak self] in self?.table.wantsAlternatingRows = $0 })
    }

    func stop() {
        observations.cancelAll()
        columns?.flush()
    }

    // MARK: Rows

    private func applyRows(_ next: [FileNode]) {
        guard next.count != rows.count || zip(next, rows).contains(where: { $0 !== $1 }) else { return }
        let previous = rows
        rows = next
        if previous.count != next.count {
            isApplyingSelection = true
            table.reloadData()
            isApplyingSelection = false
        } else {
            var moved = IndexSet()
            for (index, pair) in zip(next, previous).enumerated() where pair.0 !== pair.1 { moved.insert(index) }
            refresh(rows: moved)
        }
        applySelection(tree.selection)
    }

    private func refreshAll() {
        refresh(rows: IndexSet(integersIn: 0 ..< rows.count))
    }

    private func refresh(rows indexes: IndexSet) {
        guard !indexes.isEmpty else { return }
        let columns = table.tableColumns
        table.enumerateAvailableRowViews { rowView, index in
            guard indexes.contains(index), index < rows.count else { return }
            for (position, column) in columns.enumerated() where !column.isHidden {
                guard let kind = FileColumn(rawValue: column.identifier.rawValue),
                      let view = rowView.view(atColumn: position) as? NSView else { continue }
                configure(view, column: kind, node: rows[index])
            }
        }
    }

    private func nodes(_ indexes: IndexSet) -> [FileNode] {
        indexes.compactMap { $0 < rows.count ? rows[$0] : nil }
    }

    // MARK: Cells

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let tableColumn, row < rows.count, let column = FileColumn(rawValue: tableColumn.identifier.rawValue) else { return nil }
        let view: NSView
        switch column {
        case .name:
            let cell = FileNameCell.make(in: tableView)
            cell.onToggleExpanded = { [weak self] node in self?.tree.toggleExpanded(node) }
            cell.onToggleChecked = { [weak self] node in
                guard let self else { return }
                apply(tree.toggleChecked([node]))
            }
            view = cell
        case .priority:
            let cell = FilePriorityMenu.make(in: tableView)
            cell.onSelect = { [weak self] node, choice in
                guard let self else { return }
                apply(tree.setPriority(choice, for: [node]))
            }
            view = cell
        case .progress:
            view = TableProgressCell.make(in: tableView, identifier: Self.progressID)
        default:
            view = TableTextCell.make(in: tableView, identifier: Self.textID)
        }
        configure(view, column: column, node: rows[row])
        return view
    }

    private func configure(_ view: NSView, column: FileColumn, node: FileNode) {
        switch column {
        case .name:
            (view as? FileNameCell)?.configure(node, filtering: !tree.filterText.isEmpty)
        case .priority:
            (view as? FilePriorityMenu)?.configure(node)
        case .progress:
            (view as? TableProgressCell)?.configure(TableProgressCell.Content(progress: node.size == 0 ? 1 : node.progress,
                                                                              isDimmed: node.priority == .ignored))
        case .size:
            (view as? TableTextCell)?.configure(TableCellContent(UnitFormatter.size(node.size), monospacedDigits: true, alignment: .right))
        case .remaining:
            (view as? TableTextCell)?.configure(TableCellContent(UnitFormatter.size(node.remaining), monospacedDigits: true, alignment: .right))
        case .availability:
            let text = node.availability < 0 ? String(localized: "N/A") : ValueFormatter.progress(node.availability)
            (view as? TableTextCell)?.configure(TableCellContent(text, monospacedDigits: true, alignment: .right))
        }
    }

    func tableView(_ tableView: NSTableView, typeSelectStringFor tableColumn: NSTableColumn?, row: Int) -> String? {
        guard tableColumn?.identifier.rawValue == FileColumn.name.rawValue, row < rows.count else { return nil }
        return rows[row].name
    }

    private func apply(_ changes: [FilePriority: [Int64]]) {
        guard !changes.isEmpty else { return }
        configuration.apply(changes)
    }

    // MARK: Keys

    private func handleKey(_ event: NSEvent) -> Bool {
        guard event.modifierFlags.isDisjoint(with: [.command, .option, .control]) else { return false }
        switch event.specialKey {
        case .rightArrow?:
            tree.setExpanded(true, for: tree.selectedNodes)
            return true
        case .leftArrow?:
            tree.setExpanded(false, for: tree.selectedNodes)
            return true
        default:
            guard event.charactersIgnoringModifiers == " " else { return false }
            apply(tree.toggleChecked(tree.selectedNodes))
            return true
        }
    }

    // MARK: Selection and sorting

    private func applySelection(_ selection: Set<String>) {
        var indexes = IndexSet()
        if !selection.isEmpty {
            for (index, node) in rows.enumerated() where selection.contains(node.id) { indexes.insert(index) }
        }
        guard indexes != table.selectedRowIndexes else { return }
        isApplyingSelection = true
        table.selectRowIndexes(indexes, byExtendingSelection: false)
        isApplyingSelection = false
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isApplyingSelection else { return }
        let selection = Set(nodes(table.selectedRowIndexes).map(\.id))
        if selection != tree.selection { tree.selection = selection }
    }

    private func applySortIndicators(_ order: [FileSort]) {
        let descriptors = order.map { NSSortDescriptor(key: $0.column.rawValue, ascending: $0.order == .forward) }
        guard descriptors != table.sortDescriptors else { return }
        isApplyingSort = true
        table.sortDescriptors = descriptors
        isApplyingSort = false
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard !isApplyingSort else { return }
        let order = tableView.sortDescriptors.compactMap { descriptor -> FileSort? in
            guard let key = descriptor.key, let column = FileColumn(rawValue: key) else { return nil }
            return FileSort(column, order: descriptor.ascending ? .forward : .reverse)
        }
        if order != tree.sortOrder { tree.sortOrder = order }
    }
}
