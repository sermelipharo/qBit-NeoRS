import AppKit

/// The transfer list as an `NSTableView`: every qBittorrent column
/// (show/hide/reorder/resize persisted), sorting by any column,
/// multi-selection, context menu, double-click/Return action, drag to sidebar
/// categories/tags, and drops of .torrent files and links. Driven by
/// `TransferListModel`: a sync redraws only the cells of changed torrents.
@MainActor
final class TransferTableController: NSObject, NSTableViewDataSource, NSTableViewDelegate {
    static let layoutKey = GUIPreferenceKey.transferListColumns.rawValue

    let model: AppModel
    let table = NativeTableView()
    let scrollView = NSScrollView()
    private var columns: TableColumnsController?
    private let observations = ObservationTasks()
    private var cells: TransferCell { TransferCell(gui: model.gui, labels: model.labels, names: model.names) }

    private var items: [TorrentItem] = []
    private var rowByID: [String: Int] = [:]
    private var seenRevision = 0
    private var isApplyingSelection = false
    private var isApplyingSort = false

    init(model: AppModel) {
        self.model = model
        super.init()
        configureTable()
        columns = TableColumnsController(table: table, specs: TransferColumn.allCases.map(\.spec), storageKey: Self.layoutKey)
        items = model.list.visibleItems
        seenRevision = model.list.rowsRevision
        rebuildIndex()
        table.dataSource = self
        table.delegate = self
        table.reloadData()
        applySortIndicators(model.list.sortOrder)
        applySelection(model.list.selection, scroll: false)
    }

    private func configureTable() {
        table.identifier = NSUserInterfaceItemIdentifier("TransferList")
        table.setAccessibilityLabel(String(localized: "Transfers"))
        table.style = .inset
        table.allowsMultipleSelection = true
        table.allowsColumnReordering = true
        table.allowsColumnResizing = true
        table.allowsColumnSelection = false
        table.allowsTypeSelect = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        table.registerForDraggedTypes([.fileURL, .URL, .string])
        table.setDraggingSourceOperationMask(.copy, forLocal: true)
        table.setDraggingSourceOperationMask(.copy, forLocal: false)
        table.menuProvider = { [weak self] rows in self?.menu(for: rows) }
        table.primaryAction = { [weak self] rows in
            guard let self else { return }
            model.performDoubleClick(ids(rows))
        }
        table.deleteAction = { [weak self] shift in
            guard let self else { return }
            model.requestRemove(model.list.selectedIDs, deleteFiles: shift)
        }
        table.pasteAction = { [weak self] in self?.model.pasteFromClipboard() }
        table.keyHandler = { [weak self] event in self?.handleKey(event) ?? false }

        scrollView.documentView = table
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
    }

    // MARK: Model observation

    func start() {
        let list = model.list
        let gui = model.gui
        let labels = model.labels
        let app = model
        observations.observe({ list.visibleItems }, apply: { [weak self] in self?.applyItems($0) })
        observations.observe({ list.selection }, apply: { [weak self] in self?.applySelection($0, scroll: true) })
        observations.observe({ list.sortOrder }, apply: { [weak self] in self?.applySortIndicators($0) })
        observations.observe({ list.rowsRevision }, apply: { [weak self] in self?.applyRowChanges($0) })
        observations.observe({ gui.alternatingRowColors }, apply: { [weak self] in self?.table.wantsAlternatingRows = $0 })
        observations.observe({ app.queueingEnabled }, apply: { [weak self] in
            self?.columns?.setAvailable($0, column: TransferColumn.queuePosition.rawValue)
        })
        observations.observe({
            DisplaySettings(scope: gui.stateColorScope, hideZero: gui.hideZeroValues, hideZeroMode: gui.hideZeroMode,
                            categories: labels.categories, tags: labels.tags)
        }, apply: { [weak self] _ in self?.refreshAllRows() })
        let names = model.names
        observations.observe({ names.revision }, apply: { [weak self] _ in self?.refreshAllRows() })
    }

    func stop() {
        model.list.savedScrollPosition = scrollPosition()
        observations.cancelAll()
        columns?.flush()
    }

    // MARK: Scroll position

    private func scrollPosition() -> TransferScrollPosition? {
        let visible = scrollView.contentView.bounds
        // The first row whose bottom is below the top edge (the header overlaps the clip view's top).
        let top = visible.minY + scrollView.contentInsets.top
        let row = table.row(at: NSPoint(x: visible.midX, y: top))
        guard row >= 0, row < items.count else { return nil }
        return TransferScrollPosition(topID: items[row].id, offset: top - table.rect(ofRow: row).minY, x: visible.minX)
    }

    /// Scrolls the rebuilt table back to where the previous one was (once it has its size).
    func restoreScrollPosition() {
        // Read on the next turn: when switching from the cards view, it saves its position as it goes away,
        // which can be after this view is made.
        DispatchQueue.main.async { [weak self] in
            guard let self, let saved = model.list.savedScrollPosition else { return }
            model.list.savedScrollPosition = nil
            guard let row = rowByID[saved.topID] else { return }
            scrollView.layoutSubtreeIfNeeded()
            let clip = scrollView.contentView
            let y = table.rect(ofRow: row).minY + saved.offset - scrollView.contentInsets.top
            clip.scroll(to: clip.constrainBoundsRect(NSRect(origin: NSPoint(x: saved.x, y: y), size: clip.bounds.size)).origin)
            scrollView.reflectScrolledClipView(clip)
        }
    }

    /// Everything that changes how cells look without changing the rows.
    private struct DisplaySettings: Sendable, Equatable {
        var scope: StateColorScope
        var hideZero: Bool
        var hideZeroMode: HideZeroMode
        var categories: [String: LabelAppearance]
        var tags: [String: LabelAppearance]
    }

    // MARK: Rows

    private func rebuildIndex() {
        rowByID.removeAll(keepingCapacity: true)
        for (row, item) in items.enumerated() { rowByID[item.id] = row }
    }

    private func applyItems(_ next: [TorrentItem]) {
        guard next.count != items.count || zip(next, items).contains(where: { $0 !== $1 }) else { return }
        let previous = items
        items = next
        rebuildIndex()
        if previous.count != next.count {
            // Row indexes are stale until the model's selection is applied again below.
            isApplyingSelection = true
            table.reloadData()
            isApplyingSelection = false
        } else {
            // Same rows in another order (sorting by a changing value): redraw the rows that moved.
            var moved = IndexSet()
            for (row, pair) in zip(next, previous).enumerated() where pair.0 !== pair.1 { moved.insert(row) }
            refresh(rows: moved)
        }
        applySelection(model.list.selection, scroll: false)
    }

    private func applyRowChanges(_ revision: Int) {
        guard revision != seenRevision else { return }
        defer { seenRevision = revision }
        // Missed a sync (two in one turn): its torrents are unknown, so redraw every visible row.
        guard revision == seenRevision + 1 else {
            refreshAllRows()
            return
        }
        var rows = IndexSet()
        for id in model.list.changedIDs {
            if let row = rowByID[id] { rows.insert(row) }
        }
        refresh(rows: rows)
    }

    private func refreshAllRows() {
        refresh(rows: IndexSet(integersIn: 0 ..< items.count))
    }

    /// Reconfigures the realized cells of `rows`; cells compare their content, so only changed ones redraw.
    private func refresh(rows: IndexSet) {
        guard !rows.isEmpty else { return }
        let cells = cells
        let columns = table.tableColumns
        table.enumerateAvailableRowViews { rowView, row in
            guard rows.contains(row), row < items.count else { return }
            let item = items[row]
            for (index, tableColumn) in columns.enumerated() where !tableColumn.isHidden {
                guard let column = TransferColumn(rawValue: tableColumn.identifier.rawValue),
                      let view = rowView.view(atColumn: index) as? NSView else { continue }
                cells.configure(view, column: column, item: item)
            }
        }
    }

    private func ids(_ rows: IndexSet) -> [String] {
        rows.compactMap { $0 < items.count ? items[$0].id : nil }
    }

    // MARK: Selection and sorting

    private func applySelection(_ selection: Set<String>, scroll: Bool) {
        var rows = IndexSet()
        for id in selection {
            if let row = rowByID[id] { rows.insert(row) }
        }
        guard rows != table.selectedRowIndexes else { return }
        isApplyingSelection = true
        table.selectRowIndexes(rows, byExtendingSelection: false)
        isApplyingSelection = false
        // A selection made elsewhere (Command Palette) is scrolled into view.
        if scroll, let first = rows.first { table.scrollRowToVisible(first) }
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !isApplyingSelection else { return }
        let selection = Set(ids(table.selectedRowIndexes))
        if selection != model.list.selection { model.list.selection = selection }
    }

    private func applySortIndicators(_ order: [TransferSort]) {
        let descriptors = order.map { NSSortDescriptor(key: $0.column.rawValue, ascending: $0.order == .forward) }
        guard descriptors != table.sortDescriptors else { return }
        isApplyingSort = true
        table.sortDescriptors = descriptors
        isApplyingSort = false
    }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        guard !isApplyingSort else { return }
        let order = tableView.sortDescriptors.compactMap { descriptor -> TransferSort? in
            guard let key = descriptor.key, let column = TransferColumn(rawValue: key) else { return nil }
            return TransferSort(column, order: descriptor.ascending ? .forward : .reverse)
        }
        if order != model.list.sortOrder { model.list.sortOrder = order }
    }

    // MARK: Data source and delegate

    func numberOfRows(in tableView: NSTableView) -> Int { items.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let tableColumn, row < items.count,
              let column = TransferColumn(rawValue: tableColumn.identifier.rawValue) else { return nil }
        return cells.view(for: column, item: items[row], in: tableView)
    }

    func tableView(_ tableView: NSTableView, typeSelectStringFor tableColumn: NSTableColumn?, row: Int) -> String? {
        guard tableColumn?.identifier.rawValue == TransferColumn.name.rawValue, row < items.count else { return nil }
        return items[row].name
    }

    // MARK: Menu and keys

    private func menu(for rows: IndexSet) -> NSMenu? {
        let targets = rows.compactMap { $0 < items.count ? items[$0] : nil }
        guard !targets.isEmpty else { return nil }
        return TransferContextMenu(selection: TorrentSelection(model: model, items: targets)).makeMenu()
    }

    private func handleKey(_ event: NSEvent) -> Bool {
        guard event.specialKey == .f2, model.list.selection.count == 1, let id = model.list.selection.first else { return false }
        model.requestRename(id)
        return true
    }

    // MARK: Drag and drop

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
        guard row < items.count else { return nil }
        // One item per dragged row; the sidebar decodes and merges them.
        let item = NSPasteboardItem()
        item.setString(TorrentDragPayload.encode([items[row].id]), forType: .string)
        return item
    }

    func tableView(_ tableView: NSTableView, validateDrop info: any NSDraggingInfo, proposedRow row: Int,
                   proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
        guard info.draggingSource as? NSTableView !== tableView, !Self.droppedItems(info).isEmpty else { return [] }
        // The whole list is the target, not a row.
        tableView.setDropRow(-1, dropOperation: .on)
        return .copy
    }

    func tableView(_ tableView: NSTableView, acceptDrop info: any NSDraggingInfo, row: Int,
                   dropOperation: NSTableView.DropOperation) -> Bool {
        model.handleDrop(Self.droppedItems(info))
    }

    /// File and web URLs, or else plain text (magnet links), of a drag.
    static func droppedItems(_ info: any NSDraggingInfo) -> [DroppedItem] {
        let pasteboard = info.draggingPasteboard
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: nil) as? [URL], !urls.isEmpty {
            return urls.map(DroppedItem.url)
        }
        if let text = pasteboard.string(forType: .string), !text.isEmpty, TorrentDragPayload.decode([text]).isEmpty {
            return [.text(text)]
        }
        return []
    }
}
