import AppKit

/// The transfer list as cards (qBit NeoRS extension): an `NSCollectionView`
/// with reused card views, adaptive columns, the list's selection, sorting,
/// filters and search (all from `TransferListModel`), its context menu,
/// double-click/Return action, Delete, F2 rename, drag to sidebar categories
/// and tags, and drops of .torrent files and links. A sync reconfigures only
/// the realized cards of changed torrents; cards skip identical content.
/// Cards are as tall as their content (`TransferCardHeights`); a sync
/// re-lays out the grid only when a changed torrent's title or tags changed.
@MainActor
final class TransferCardsController: NSObject, NSCollectionViewDataSource, NSCollectionViewDelegateFlowLayout {
    let model: AppModel
    let collectionView = TransferCardsCollectionView()
    let scrollView = NSScrollView()
    private let layout = TransferCardsLayout()
    private let heights = TransferCardHeights()
    private var ids: [String] = []
    private let observations = ObservationTasks()
    private var builder: TransferCardBuilder { TransferCardBuilder(gui: model.gui, labels: model.labels, names: model.names) }

    private var items: [TorrentItem] = []
    private var indexByID: [String: Int] = [:]
    private var seenRevision = 0
    private var isApplyingSelection = false

    init(model: AppModel) {
        self.model = model
        super.init()
        configure()
        items = model.list.visibleItems
        seenRevision = model.list.rowsRevision
        rebuildIndex()
        collectionView.reloadData()
        applySelection(model.list.selection, scroll: false)
    }

    private func configure() {
        collectionView.identifier = NSUserInterfaceItemIdentifier("TransferCards")
        collectionView.setAccessibilityLabel(String(localized: "Transfers"))
        collectionView.collectionViewLayout = layout
        collectionView.isSelectable = true
        collectionView.allowsMultipleSelection = true
        collectionView.allowsEmptySelection = true
        collectionView.backgroundColors = [.controlBackgroundColor]
        collectionView.register(TransferCardItem.self, forItemWithIdentifier: TransferCardItem.identifier)
        collectionView.registerForDraggedTypes([.fileURL, .URL, .string])
        collectionView.setDraggingSourceOperationMask(.copy, forLocal: true)
        collectionView.setDraggingSourceOperationMask(.copy, forLocal: false)
        collectionView.dataSource = self
        collectionView.delegate = self
        collectionView.menuProvider = { [weak self] paths in self?.menu(for: paths) }
        collectionView.primaryAction = { [weak self] paths in
            guard let self else { return }
            model.performDoubleClick(ids(paths))
        }
        collectionView.deleteAction = { [weak self] shift in
            guard let self else { return }
            model.requestRemove(model.list.selectedIDs, deleteFiles: shift)
        }
        collectionView.keyHandler = { [weak self] event in self?.handleKey(event) ?? false }
        collectionView.onEmphasisChange = { [weak self] emphasized in self?.applyEmphasis(emphasized) }

        scrollView.documentView = collectionView
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = true
        scrollView.backgroundColor = .controlBackgroundColor
        scrollView.contentView.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(self, selector: #selector(clipFrameChanged), name: NSView.frameDidChangeNotification,
                                               object: scrollView.contentView)
    }

    private var laidOutWidth: CGFloat = 0

    /// The flow layout keeps the delegate's sizes across a resize: ask again when the width changes.
    @objc private func clipFrameChanged() {
        let width = scrollView.contentView.bounds.width
        guard width != laidOutWidth else { return }
        laidOutWidth = width
        layout.invalidateLayout()
    }

    // MARK: Model observation

    func start() {
        let list = model.list
        let gui = model.gui
        let labels = model.labels
        let names = model.names
        observations.observe({ list.visibleItems }, apply: { [weak self] in self?.applyItems($0) })
        observations.observe({ list.selection }, apply: { [weak self] in self?.applySelection($0, scroll: true) })
        observations.observe({ list.rowsRevision }, apply: { [weak self] in self?.applyRowChanges($0) })
        observations.observe({ names.revision }, apply: { [weak self] _ in self?.namesChanged() })
        observations.observe({
            DisplaySettings(scope: gui.stateColorScope, categories: labels.categories, tags: labels.tags)
        }, apply: { [weak self] _ in self?.relayoutAll() })
    }

    func stop() {
        model.list.savedScrollPosition = scrollPosition()
        observations.cancelAll()
    }

    private struct DisplaySettings: Sendable, Equatable {
        var scope: StateColorScope
        var categories: [String: LabelAppearance]
        var tags: [String: LabelAppearance]
    }

    // MARK: Scroll position

    private func scrollPosition() -> TransferScrollPosition? {
        let visible = scrollView.contentView.bounds
        let top = visible.minY + scrollView.contentInsets.top
        let paths = collectionView.indexPathsForVisibleItems().sorted()
        guard let first = paths.first(where: { (layout.layoutAttributesForItem(at: $0)?.frame.maxY ?? 0) > top }),
              first.item < items.count, let frame = layout.layoutAttributesForItem(at: first)?.frame
        else { return nil }
        return TransferScrollPosition(topID: items[first.item].id, offset: top - frame.minY, x: 0)
    }

    /// Scrolls the rebuilt view back to where the previous one (list or cards) was.
    func restoreScrollPosition() {
        // Read on the next turn: the list saves its position as it goes away, which can be after this view is made.
        DispatchQueue.main.async { [weak self] in
            guard let self, let saved = model.list.savedScrollPosition else { return }
            model.list.savedScrollPosition = nil
            guard let index = indexByID[saved.topID] else { return }
            scrollView.layoutSubtreeIfNeeded()
            collectionView.layoutSubtreeIfNeeded()
            guard let frame = layout.layoutAttributesForItem(at: IndexPath(item: index, section: 0))?.frame else { return }
            let clip = scrollView.contentView
            let y = frame.minY + min(saved.offset, frame.height) - scrollView.contentInsets.top
            clip.scroll(to: clip.constrainBoundsRect(NSRect(origin: NSPoint(x: 0, y: y), size: clip.bounds.size)).origin)
            scrollView.reflectScrolledClipView(clip)
        }
    }

    // MARK: Items

    private func rebuildIndex() {
        indexByID.removeAll(keepingCapacity: true)
        for (index, item) in items.enumerated() { indexByID[item.id] = index }
        ids = items.map(\.id)
    }

    private func applyItems(_ next: [TorrentItem]) {
        guard next.count != items.count || zip(next, items).contains(where: { $0 !== $1 }) else { return }
        let previous = items
        items = next
        rebuildIndex()
        heights.retain(Set(ids))
        heights.invalidateRows()
        isApplyingSelection = true
        if previous.count != next.count {
            collectionView.reloadData()
        } else {
            layout.invalidateLayout()
            // Same cards in another order: reconfigure the positions that changed.
            var moved = Set<Int>()
            for (index, pair) in zip(next, previous).enumerated() where pair.0 !== pair.1 { moved.insert(index) }
            refresh(moved)
        }
        isApplyingSelection = false
        applySelection(model.list.selection, scroll: false)
    }

    private func applyRowChanges(_ revision: Int) {
        guard revision != seenRevision else { return }
        defer { seenRevision = revision }
        guard revision == seenRevision + 1 else {
            refreshAll()
            return
        }
        let changed = model.list.changedIDs
        let builder = builder
        // A changed name, category or tag can change a card's height.
        var relayout = false
        for id in changed {
            guard let index = indexByID[id] else { continue }
            if heights.update(id, input: builder.layoutInput(items[index])) { relayout = true }
        }
        if relayout { layout.invalidateLayout() }
        refresh(Set(changed.compactMap { indexByID[$0] }))
    }

    private func namesChanged() {
        relayoutAll()
    }

    /// Names, labels or their display changed: new heights and contents for every card.
    private func relayoutAll() {
        heights.removeAll()
        layout.invalidateLayout()
        refreshAll()
    }

    private func refreshAll() {
        refresh(nil)
    }

    /// Reconfigures the realized cards at `indexes` (nil: all of them).
    private func refresh(_ indexes: Set<Int>?) {
        if let indexes, indexes.isEmpty { return }
        let builder = builder
        let emphasized = collectionView.isEmphasized
        for path in collectionView.indexPathsForVisibleItems() where indexes?.contains(path.item) ?? true {
            guard path.item < items.count, let item = collectionView.item(at: path) as? TransferCardItem else { continue }
            configure(item, with: items[path.item], builder: builder, emphasized: emphasized)
        }
    }

    private func configure(_ item: TransferCardItem, with torrent: TorrentItem, builder: TransferCardBuilder, emphasized: Bool) {
        let card = item.card
        card.isEmphasized = emphasized
        card.content = builder.content(torrent)
    }

    private func applyEmphasis(_ emphasized: Bool) {
        for case let item as TransferCardItem in collectionView.visibleItems() {
            item.card.isEmphasized = emphasized
        }
    }

    private func ids(_ paths: Set<IndexPath>) -> [String] {
        paths.sorted().compactMap { $0.item < items.count ? items[$0.item].id : nil }
    }

    // MARK: Selection

    private func applySelection(_ selection: Set<String>, scroll: Bool) {
        let paths = Set(selection.compactMap { indexByID[$0].map { IndexPath(item: $0, section: 0) } })
        guard paths != collectionView.selectionIndexPaths else { return }
        isApplyingSelection = true
        collectionView.selectionIndexPaths = paths
        isApplyingSelection = false
        if scroll, let first = paths.min() {
            collectionView.scrollToItems(at: [first], scrollPosition: .nearestHorizontalEdge)
        }
    }

    private func selectionDidChange() {
        guard !isApplyingSelection else { return }
        let selection = Set(ids(collectionView.selectionIndexPaths))
        if selection != model.list.selection { model.list.selection = selection }
    }

    func collectionView(_ collectionView: NSCollectionView, didSelectItemsAt indexPaths: Set<IndexPath>) {
        selectionDidChange()
    }

    func collectionView(_ collectionView: NSCollectionView, didDeselectItemsAt indexPaths: Set<IndexPath>) {
        selectionDidChange()
    }

    // MARK: Data source

    func collectionView(_ collectionView: NSCollectionView, numberOfItemsInSection section: Int) -> Int {
        items.count
    }

    func collectionView(_ collectionView: NSCollectionView, itemForRepresentedObjectAt indexPath: IndexPath) -> NSCollectionViewItem {
        let item = collectionView.makeItem(withIdentifier: TransferCardItem.identifier, for: indexPath)
        if let card = item as? TransferCardItem, indexPath.item < items.count {
            configure(card, with: items[indexPath.item], builder: builder, emphasized: self.collectionView.isEmphasized)
        }
        return item
    }

    // MARK: Layout

    func collectionView(
        _ collectionView: NSCollectionView, layout collectionViewLayout: NSCollectionViewLayout, sizeForItemAt indexPath: IndexPath
    ) -> NSSize {
        let width = layout.cardWidth
        let builder = builder
        let items = items
        let height = heights.rowHeight(at: indexPath.item, ids: ids, columns: layout.columnCount, cardWidth: width) {
            builder.layoutInput(items[$0])
        }
        return NSSize(width: width, height: height)
    }

    // MARK: Menu and keys

    private func menu(for paths: Set<IndexPath>) -> NSMenu? {
        let targets = paths.sorted().compactMap { $0.item < items.count ? items[$0.item] : nil }
        guard !targets.isEmpty else { return nil }
        return TransferContextMenu(selection: TorrentSelection(model: model, items: targets)).makeMenu()
    }

    private func handleKey(_ event: NSEvent) -> Bool {
        guard event.specialKey == .f2, model.list.selection.count == 1, let id = model.list.selection.first else { return false }
        model.requestRename(id)
        return true
    }

    // MARK: Drag and drop

    func collectionView(_ collectionView: NSCollectionView, canDragItemsAt indexPaths: Set<IndexPath>, with event: NSEvent) -> Bool {
        true
    }

    func collectionView(_ collectionView: NSCollectionView, pasteboardWriterForItemAt indexPath: IndexPath) -> (any NSPasteboardWriting)? {
        guard indexPath.item < items.count else { return nil }
        // One item per dragged card; the sidebar decodes and merges them.
        let item = NSPasteboardItem()
        item.setString(TorrentDragPayload.encode([items[indexPath.item].id]), forType: .string)
        return item
    }

    func collectionView(
        _ collectionView: NSCollectionView, validateDrop draggingInfo: any NSDraggingInfo,
        proposedIndexPath proposedDropIndexPath: AutoreleasingUnsafeMutablePointer<NSIndexPath>,
        dropOperation proposedDropOperation: UnsafeMutablePointer<NSCollectionView.DropOperation>
    ) -> NSDragOperation {
        guard draggingInfo.draggingSource as? NSCollectionView !== collectionView,
              !TransferTableController.droppedItems(draggingInfo).isEmpty else { return [] }
        return .copy
    }

    func collectionView(
        _ collectionView: NSCollectionView, acceptDrop draggingInfo: any NSDraggingInfo, indexPath: IndexPath,
        dropOperation: NSCollectionView.DropOperation
    ) -> Bool {
        model.handleDrop(TransferTableController.droppedItems(draggingInfo))
    }
}
