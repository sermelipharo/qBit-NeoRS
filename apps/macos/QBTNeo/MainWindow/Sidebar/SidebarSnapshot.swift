import AppKit

/// Everything the filters sidebar shows, read from the models in one pass:
/// sections, rows (icon, title, count) and the category tree. Value types
/// only, so the sidebar can observe it and compare it with what it shows.
struct SidebarSnapshot: Equatable, Sendable {
    enum Tint: Equatable, Sendable {
        case secondary
        case accent
        case orange
        case state(TorrentState)
        case label(LabelColor?, LabelAppearance.Kind)

        @MainActor
        var color: NSColor {
            switch self {
            case .secondary: .secondaryLabelColor
            case .accent: .controlAccentColor
            case .orange: .systemOrange
            case .state(let state): state.nsColor
            case let .label(color, kind): color?.nsColor ?? kind.defaultNSColor
            }
        }
    }

    struct Row: Equatable, Sendable {
        let item: SidebarItem
        var title: String
        var symbol: String
        var tint: Tint = .secondary
        var count: Int
        /// The tracker host whose favicon replaces the symbol, when one is loaded.
        var faviconHost: String?
        var children: [Self] = []
    }

    struct Section: Equatable, Sendable {
        let id: SidebarItem.Section
        let title: String
        var isExpanded: Bool
        var rows: [Row]
    }

    /// The rows above the filters that switch the main window's tab (Search).
    struct PaneRow: Equatable, Sendable {
        let pane: MainPane
        var count: Int
    }

    /// An RSS folder or feed below the filters, or the "Manage Feeds" row
    /// (`id` empty) while there are none. Selecting one opens the RSS tab.
    struct FeedRow: Equatable, Sendable {
        static let manageID = ""
        let id: String
        var title: String
        var symbol: String
        var tint: Tint
        var count: Int
        var children: [Self] = []
    }

    struct FeedSection: Equatable, Sendable {
        var isExpanded: Bool
        var rows: [FeedRow]
    }

    var panes: [PaneRow]
    var sections: [Section]
    /// Nil when hidden.
    var feeds: FeedSection?

    // MARK: Building

    @MainActor
    init(model: AppModel) {
        let gui = model.gui
        let counts = model.list.counts
        // Transfers have no row of their own: any filter row shows them.
        panes = [PaneRow(pane: .search, count: 0)]
        var sections = [
            Section(id: .status, title: String(localized: "Status"), isExpanded: gui.statusSectionExpanded, rows: Self.statusRows(model, counts)),
            Section(id: .category, title: String(localized: "Categories"), isExpanded: gui.categorySectionExpanded,
                    rows: Self.categoryRows(model, counts)),
            Section(id: .tag, title: String(localized: "Tags"), isExpanded: gui.tagSectionExpanded, rows: Self.tagRows(model, counts)),
        ]
        if gui.separateTrackerStatusFilter {
            sections.append(Section(id: .trackerStatus, title: String(localized: "Tracker Status"),
                                    isExpanded: gui.trackerStatusSectionExpanded, rows: Self.trackerStatusRows(counts)))
        }
        sections.append(Section(id: .tracker, title: String(localized: "Trackers"), isExpanded: gui.trackerSectionExpanded,
                                rows: Self.trackerRows(model, counts)))
        self.sections = sections.filter { !gui.hiddenSidebarSections.contains($0.id) }
        feeds = gui.hiddenSidebarSections.contains(.rss) ? nil
            : FeedSection(isExpanded: gui.rssSectionExpanded, rows: Self.feedRows(model.rssTab.nodes))
    }

    private static func feedRows(_ nodes: [RSSNode]) -> [FeedRow] {
        func row(_ node: RSSNode) -> FeedRow {
            let symbol = node.kind == .folder ? Symbol.folder : node.hasError ? Symbol.exclamationmarkTriangleFill : Symbol.dotRadiowavesUpForward
            return FeedRow(id: node.id, title: node.name, symbol: symbol, tint: node.kind == .folder ? .accent : .orange,
                           count: Int(node.unreadCount), children: (node.children ?? []).map(row))
        }
        let rows = nodes.filter { $0.kind != .unread }.map(row)
        guard rows.isEmpty else { return rows }
        return [FeedRow(id: FeedRow.manageID, title: String(localized: "Manage Feeds"), symbol: Symbol.dotRadiowavesUpForward,
                        tint: .orange, count: 0)]
    }

    @MainActor
    private static func statusRows(_ model: AppModel, _ counts: SidebarCounts) -> [Row] {
        var filters = StatusFilter.ordered
        if model.gui.hideZeroStatusFilters {
            filters = filters.filter { $0 == .all || counts.count(for: $0) > 0 || $0 == model.list.statusFilter }
        }
        return filters.map { filter in
            Row(item: .status(filter), title: filter.title, symbol: filter.systemImage, tint: filter.sidebarTint,
                count: counts.count(for: filter))
        }
    }

    @MainActor
    private static func categoryRows(_ model: AppModel, _ counts: SidebarCounts) -> [Row] {
        func row(_ node: CategoryNode) -> Row {
            let appearance = model.labels.category(node.id)
            return Row(item: .category(.category(node.id)), title: node.name, symbol: appearance.resolvedSymbol(.category),
                       tint: .label(appearance.color, .category), count: counts.categories[node.id] ?? 0,
                       children: (node.children ?? []).map(row))
        }
        return [
            Row(item: .category(.all), title: String(localized: "All"), symbol: Symbol.folder, count: counts.total),
            Row(item: .category(.uncategorized), title: String(localized: "Uncategorized"), symbol: Symbol.folderBadgeQuestionmark,
                count: counts.uncategorized),
        ] + model.categories.tree.map(row)
    }

    @MainActor
    private static func tagRows(_ model: AppModel, _ counts: SidebarCounts) -> [Row] {
        [
            Row(item: .tag(.all), title: String(localized: "All"), symbol: Symbol.tag, count: counts.total),
            Row(item: .tag(.untagged), title: String(localized: "Untagged"), symbol: Symbol.tagSlash, count: counts.untagged),
        ] + model.tags.sortedTags.map { tag in
            let appearance = model.labels.tag(tag)
            return Row(item: .tag(.tag(tag)), title: tag, symbol: appearance.resolvedSymbol(.tag), tint: .label(appearance.color, .tag),
                       count: counts.tags[tag] ?? 0)
        }
    }

    private static func trackerStatusRows(_ counts: SidebarCounts) -> [Row] {
        TrackerStatusFilter.allCases.map { filter in
            let count = switch filter {
            case .all: counts.total
            case .warning: counts.warning
            case .trackerError: counts.trackerError
            case .otherError: counts.otherError
            }
            return Row(item: .trackerStatus(filter), title: filter.title, symbol: filter.systemImage, count: count)
        }
    }

    @MainActor
    private static func trackerRows(_ model: AppModel, _ counts: SidebarCounts) -> [Row] {
        var rows = [
            Row(item: .tracker(.all), title: String(localized: "All"), symbol: Symbol.antennaRadiowavesLeftAndRight, count: counts.total),
            Row(item: .tracker(.trackerless), title: String(localized: "Trackerless"), symbol: Symbol.antennaRadiowavesLeftAndRightSlash,
                count: counts.trackerless),
        ]
        if !model.gui.separateTrackerStatusFilter {
            rows += [
                Row(item: .tracker(.trackerError), title: String(localized: "Tracker error"), symbol: Symbol.xmarkOctagon,
                    tint: .state(.error), count: counts.trackerError),
                Row(item: .tracker(.otherError), title: String(localized: "Other error"), symbol: Symbol.xmarkOctagon,
                    tint: .state(.error), count: counts.otherError),
                Row(item: .tracker(.warning), title: String(localized: "Warning"), symbol: Symbol.exclamationmarkTriangle,
                    tint: .orange, count: counts.warning),
            ]
        }
        let favicons = model.gui.downloadTrackerFavicon
        rows += model.trackers.sortedHosts.map { host in
            Row(item: .tracker(.host(host)), title: host, symbol: Symbol.serverRack, count: counts.hosts[host] ?? 0,
                faviconHost: favicons && model.favicons.icon(for: host) != nil ? host : nil)
        }
        return rows
    }

    enum Node: Equatable {
        case pane(MainPane)
        case section(SidebarItem.Section)
        case row(SidebarItem, depth: Int)
        case feed(String, depth: Int)
    }

    /// Sections and rows in display order with their depth; a different list means the outline reloads.
    var structure: [Node] {
        func flatten(_ rows: [Row], depth: Int) -> [Node] {
            rows.flatMap { [Node.row($0.item, depth: depth)] + flatten($0.children, depth: depth + 1) }
        }
        func flatten(_ rows: [FeedRow], depth: Int) -> [Node] {
            rows.flatMap { [Node.feed($0.id, depth: depth)] + flatten($0.children, depth: depth + 1) }
        }
        let feedNodes = feeds.map { [Node.section(.rss)] + flatten($0.rows, depth: 0) } ?? []
        return panes.map { Node.pane($0.pane) } + sections.flatMap { [Node.section($0.id)] + flatten($0.rows, depth: 0) } + feedNodes
    }
}

extension StatusFilter {
    /// Sidebar icon color (as `tint`).
    var sidebarTint: SidebarSnapshot.Tint {
        switch self {
        case .downloading, .running, .checking, .moving: .state(.downloading)
        case .seeding, .stalledUploading: .state(.uploading)
        case .completed: .state(.stoppedUploading)
        case .stopped, .inactive: .state(.stoppedDownloading)
        case .errored: .state(.error)
        case .stalledDownloading, .stalled: .state(.stalledDownloading)
        case .all, .active: .accent
        }
    }
}
