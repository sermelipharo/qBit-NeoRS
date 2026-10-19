import AppKit

/// The filters sidebar as a source-list `NSOutlineView`: collapsible
/// sections (Status, Categories with subcategories, Tags, Tracker Status,
/// Trackers), one selected row per section with ⌘-click toggling, row
/// context menus and drops of dragged torrents on categories and tags.
/// Follows the models through `SidebarSnapshot`; count changes redraw the
/// realized rows only.
@MainActor
final class SidebarController: NSObject, NSOutlineViewDataSource, NSOutlineViewDelegate {
    let model: AppModel
    let outline = SidebarOutlineView()
    let scrollView = NSScrollView()
    private let observations = ObservationTasks()
    private var entries: [SidebarEntry.Key: SidebarEntry] = [:]
    private var roots: [SidebarEntry] = []
    private var snapshot: SidebarSnapshot?
    private var isApplying = false
    private static let headerID = NSUserInterfaceItemIdentifier("sidebar.header")

    init(model: AppModel) {
        self.model = model
        super.init()
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("filters"))
        column.resizingMask = .autoresizingMask
        outline.addTableColumn(column)
        outline.outlineTableColumn = column
        outline.headerView = nil
        outline.style = .sourceList
        outline.rowSizeStyle = .default
        outline.floatsGroupRows = false
        outline.allowsMultipleSelection = true
        outline.allowsEmptySelection = true
        outline.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        outline.autosaveExpandedItems = false
        outline.identifier = NSUserInterfaceItemIdentifier("Sidebar")
        outline.setAccessibilityLabel(String(localized: "Filters"))
        outline.registerForDraggedTypes([.string])
        outline.menuProvider = { [weak self] row in
            guard let self, let entry = outline.item(atRow: row) as? SidebarEntry else { return nil }
            if let item = entry.item { return SidebarMenu.make(for: item, model: model) }
            if let feed = entry.feed { return SidebarMenu.make(forFeed: feed, model: model) }
            if let section = entry.section { return SidebarMenu.make(forSection: section, model: model) }
            return nil
        }
        scrollView.documentView = outline
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = false
        outline.dataSource = self
        outline.delegate = self
        apply(SidebarSnapshot(model: model))
    }

    func start() {
        let model = model
        let list = model.list
        observations.observe({ SidebarSnapshot(model: model) }, apply: { [weak self] in self?.apply($0) })
        let rss = model.rssTab
        observations.observe({ (list.sidebarSelection, model.mainPane, rss.selection) }, apply: { [weak self] in self?.applySelection($0.0) })
    }

    func stop() {
        observations.cancelAll()
    }

    // MARK: Snapshot

    private func entry(_ key: SidebarEntry.Key) -> SidebarEntry {
        if let existing = entries[key] { return existing }
        let entry = SidebarEntry(key: key)
        entries[key] = entry
        return entry
    }

    private func entries(for rows: [SidebarSnapshot.Row], used: inout Set<SidebarEntry.Key>) -> [SidebarEntry] {
        rows.map { row in
            let entry = entry(.row(row.item))
            entry.title = row.title
            entry.row = row
            entry.children = entries(for: row.children, used: &used)
            used.insert(entry.key)
            return entry
        }
    }

    private func entries(for rows: [SidebarSnapshot.FeedRow], used: inout Set<SidebarEntry.Key>) -> [SidebarEntry] {
        rows.map { row in
            let entry = entry(.feed(row.id))
            entry.title = row.title
            entry.feedRow = row
            entry.children = entries(for: row.children, used: &used)
            used.insert(entry.key)
            return entry
        }
    }

    private func apply(_ next: SidebarSnapshot) {
        guard next != snapshot else { return }
        let structureChanged = snapshot?.structure != next.structure
        var used = Set<SidebarEntry.Key>()
        let paneEntries = next.panes.map { pane in
            let entry = entry(.pane(pane.pane))
            entry.title = pane.pane.title
            entry.paneRow = pane
            used.insert(entry.key)
            return entry
        }
        roots = paneEntries + next.sections.map { section in
            let entry = entry(.section(section.id))
            entry.title = section.title
            entry.children = entries(for: section.rows, used: &used)
            used.insert(entry.key)
            return entry
        }
        if let feeds = next.feeds {
            let entry = entry(.section(.rss))
            entry.title = SidebarItem.Section.rss.title
            entry.children = entries(for: feeds.rows, used: &used)
            used.insert(entry.key)
            roots.append(entry)
        }
        entries = entries.filter { used.contains($0.key) }
        snapshot = next
        isApplying = true
        if structureChanged {
            let expanded = Set(entries.values.filter { outline.isItemExpanded($0) && $0.section == nil }.map(\.key))
            outline.reloadData()
            for key in expanded { if let entry = entries[key] { outline.expandItem(entry) } }
        } else {
            refreshRows()
        }
        var expansion = next.sections.map { ($0.id, $0.isExpanded) }
        if let feeds = next.feeds { expansion.append((.rss, feeds.isExpanded)) }
        for (id, isExpanded) in expansion {
            guard let entry = entries[.section(id)] else { continue }
            if isExpanded, !outline.isItemExpanded(entry) {
                outline.expandItem(entry)
            } else if !isExpanded, outline.isItemExpanded(entry) {
                outline.collapseItem(entry)
            }
        }
        isApplying = false
        applySelection(model.list.sidebarSelection)
    }

    private func refreshRows() {
        outline.enumerateAvailableRowViews { rowView, row in
            guard let entry = outline.item(atRow: row) as? SidebarEntry, let cell = rowView.view(atColumn: 0) as? SidebarRow else { return }
            if let pane = entry.paneRow { cell.configure(pane); return }
            if let feed = entry.feedRow { cell.configure(feed); return }
            guard let content = entry.row else { return }
            cell.configure(content, favicon: favicon(content))
        }
    }

    private func favicon(_ row: SidebarSnapshot.Row) -> NSImage? {
        row.faviconHost.flatMap { model.favicons.icon(for: $0) }
    }

    // MARK: Selection

    /// Transfers (and the full-size log) select the filter rows (which keep
    /// applying on every tab), Search its row, RSS the shown feeds.
    private func applySelection(_ selection: Set<SidebarItem>) {
        let keys: [SidebarEntry.Key] = switch model.mainPane {
        case .transfers, .log: selection.map { .row($0) }
        case .search: [.pane(.search)]
        case .rss: feedKeys
        }
        var rows = IndexSet()
        for key in keys {
            if let entry = entries[key] {
                let row = outline.row(forItem: entry)
                if row >= 0 { rows.insert(row) }
            }
        }
        guard rows != outline.selectedRowIndexes else { return }
        isApplying = true
        outline.selectRowIndexes(rows, byExtendingSelection: false)
        isApplying = false
    }

    private var feedKeys: [SidebarEntry.Key] {
        if entries[.feed(SidebarSnapshot.FeedRow.manageID)] != nil { return [.feed(SidebarSnapshot.FeedRow.manageID)] }
        return model.rssTab.selection.map { .feed($0) }
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !isApplying else { return }
        let selected = outline.selectedRowIndexes.compactMap { outline.item(atRow: $0) as? SidebarEntry }
        let items = selected.compactMap(\.item)
        let feeds = selected.compactMap(\.feed)
        if let pane = selected.compactMap(\.pane).first(where: { $0 != model.mainPane }) {
            model.show(pane: pane)
        } else if !feeds.isEmpty, Set(feeds) != Set(model.mainPane == .rss ? model.rssTab.selection : []) {
            // A feed or folder opens the RSS tab on it.
            let shown = feeds.filter { model.rssTab.node($0) != nil }
            model.show(pane: .rss)
            if !shown.isEmpty { model.rssTab.selection = Set(shown) }
        } else if !items.isEmpty {
            // A filter row shows the transfers it filters.
            if model.mainPane != .transfers { model.mainPane = .transfers }
            model.list.sidebarSelection = Set(items)
        }
        // The model keeps one row per section: show what it chose.
        applySelection(model.list.sidebarSelection)
    }

    // MARK: Data source

    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        guard let entry = item as? SidebarEntry else { return roots.count }
        return entry.children.count
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        guard let entry = item as? SidebarEntry else { return roots[index] }
        return entry.children[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        guard let entry = item as? SidebarEntry else { return false }
        return entry.section != nil || !entry.children.isEmpty
    }

    // MARK: Delegate

    func outlineView(_ outlineView: NSOutlineView, isGroupItem item: Any) -> Bool {
        (item as? SidebarEntry)?.section != nil
    }

    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool {
        guard let entry = item as? SidebarEntry else { return false }
        return entry.item != nil || entry.pane != nil || entry.feed != nil
    }

    func outlineView(_ outlineView: NSOutlineView, viewFor tableColumn: NSTableColumn?, item: Any) -> NSView? {
        guard let entry = item as? SidebarEntry else { return nil }
        if let pane = entry.paneRow {
            let cell = SidebarRow.make(in: outlineView)
            cell.configure(pane)
            return cell
        }
        if let feed = entry.feedRow {
            let cell = SidebarRow.make(in: outlineView)
            cell.configure(feed)
            return cell
        }
        guard let row = entry.row else {
            let header = outlineView.makeView(withIdentifier: Self.headerID, owner: nil) as? NSTableCellView ?? Self.makeHeader()
            header.textField?.stringValue = entry.title
            return header
        }
        let cell = SidebarRow.make(in: outlineView)
        cell.configure(row, favicon: favicon(row))
        return cell
    }

    private static func makeHeader() -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = headerID
        let label = NSTextField(labelWithString: "")
        label.font = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .semibold)
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(label)
        cell.textField = label
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
            label.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func outlineViewItemDidExpand(_ notification: Notification) {
        sectionExpansionChanged(notification, expanded: true)
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        sectionExpansionChanged(notification, expanded: false)
    }

    /// Collapsing a section also resets its filter to "All" (qBittorrent hides the filter widget).
    private func sectionExpansionChanged(_ notification: Notification, expanded: Bool) {
        guard !isApplying, let section = (notification.userInfo?["NSObject"] as? SidebarEntry)?.section else { return }
        let gui = model.gui
        let list = model.list
        switch section {
        case .status:
            gui.statusSectionExpanded = expanded
            if !expanded { list.statusFilter = .all }
        case .category:
            gui.categorySectionExpanded = expanded
            if !expanded { list.categoryFilter = .all }
        case .tag:
            gui.tagSectionExpanded = expanded
            if !expanded { list.tagFilter = .all }
        case .trackerStatus:
            gui.trackerStatusSectionExpanded = expanded
            if !expanded { list.trackerStatusFilter = .all }
        case .tracker:
            gui.trackerSectionExpanded = expanded
            if !expanded { list.trackerFilter = .all }
        case .rss:
            gui.rssSectionExpanded = expanded
        }
    }

    // MARK: Drops (torrents dragged from the transfer list)

    func outlineView(_ outlineView: NSOutlineView, validateDrop info: any NSDraggingInfo, proposedItem item: Any?,
                     proposedChildIndex index: Int) -> NSDragOperation {
        guard let entry = item as? SidebarEntry, Self.acceptsDrop(entry.item), !Self.draggedIDs(info).isEmpty else { return [] }
        outlineView.setDropItem(entry, dropChildIndex: NSOutlineViewDropOnItemIndex)
        return .copy
    }

    func outlineView(_ outlineView: NSOutlineView, acceptDrop info: any NSDraggingInfo, item: Any?, childIndex index: Int) -> Bool {
        let ids = Self.draggedIDs(info)
        guard let item = (item as? SidebarEntry)?.item, !ids.isEmpty else { return false }
        switch item {
        case .category(.uncategorized): model.setCategory(ids, "")
        case .category(.category(let name)): model.setCategory(ids, name)
        case .tag(.tag(let tag)): model.perform { try $0.addTags(hashes: ids, tags: [tag]) }
        default: return false
        }
        return true
    }

    private static func acceptsDrop(_ item: SidebarItem?) -> Bool {
        switch item {
        case .category(.uncategorized)?, .category(.category)?, .tag(.tag)?: true
        default: false
        }
    }

    private static func draggedIDs(_ info: any NSDraggingInfo) -> [String] {
        let strings = info.draggingPasteboard.pasteboardItems?.compactMap { $0.string(forType: .string) } ?? []
        return TorrentDragPayload.decode(strings)
    }
}
