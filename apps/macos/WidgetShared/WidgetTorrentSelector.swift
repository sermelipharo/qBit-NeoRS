import Foundation

/// Filtering and ranking of torrents for the widgets (pure functions).
///
/// Rankings are total orders (the identifier breaks ties), so the top `k`
/// entries of any subset that contains the full set's top `k` equal the full
/// set's. The app publishes the union of the top entries of every filter, and
/// of every filter-and-category pair an installed widget uses
/// (`publishedSubset`); a widget selecting from that subset shows exactly
/// what it would show with every torrent at hand.
enum WidgetTorrentSelector {
    /// Rows the largest widget layout shows.
    static let rowsPerGroup = 6
    /// Categories offered in the widget configuration.
    static let maxCategories = 16
    /// Category scopes of installed widgets that are published.
    static let maxConfiguredScopes = 10
    /// Torrents installed Torrent widgets may pin.
    static let maxPinned = 12
    /// Entries of the Torrent widget's picker.
    static let maxCandidates = 200
    /// Hard cap on published torrents: enough for every scope `scopes(configured:)` returns.
    static let maxPublished = (WidgetTorrentFilter.allCases.count + maxConfiguredScopes) * rowsPerGroup

    static func matches(_ torrent: some WidgetRankable, filter: WidgetTorrentFilter, category: String?) -> Bool {
        if let category, torrent.category != category { return false }
        switch filter {
        case .active: return torrent.isTransferring
        case .downloading: return torrent.isDownloading
        case .seeding: return torrent.state.isSeedingState
        case .recent, .all: return true
        }
    }

    /// Whether `lhs` ranks before `rhs` for `filter`.
    static func ranks<T: WidgetRankable>(_ lhs: T, before rhs: T, filter: WidgetTorrentFilter) -> Bool {
        switch filter {
        case .active:
            if lhs.totalRate != rhs.totalRate { return lhs.totalRate > rhs.totalRate }
        case .downloading:
            if lhs.downloadRate != rhs.downloadRate { return lhs.downloadRate > rhs.downloadRate }
            if lhs.state.downloadRank != rhs.state.downloadRank { return lhs.state.downloadRank < rhs.state.downloadRank }
            if lhs.progress != rhs.progress { return lhs.progress > rhs.progress }
        case .seeding:
            if lhs.uploadRate != rhs.uploadRate { return lhs.uploadRate > rhs.uploadRate }
        case .recent:
            if lhs.lastEvent != rhs.lastEvent { return lhs.lastEvent > rhs.lastEvent }
        case .all:
            if lhs.isTransferring != rhs.isTransferring { return lhs.isTransferring }
            if lhs.totalRate != rhs.totalRate { return lhs.totalRate > rhs.totalRate }
            if lhs.isDownloading != rhs.isDownloading { return lhs.isDownloading }
            if lhs.lastEvent != rhs.lastEvent { return lhs.lastEvent > rhs.lastEvent }
        }
        return lhs.id < rhs.id
    }

    /// The first `limit` matching torrents in rank order.
    static func select<T: WidgetRankable>(_ torrents: [T], filter: WidgetTorrentFilter, category: String?, limit: Int) -> [T] {
        guard limit > 0 else { return [] }
        let matching = torrents.filter { matches($0, filter: filter, category: category) }
        return top(matching, limit: limit) { ranks($0, before: $1, filter: filter) }
    }

    /// Categories worth offering: most torrents first, then by name. Those in
    /// `required` (used by installed widgets) are always included.
    static func topCategories(_ counts: [String: WidgetCounts], required: Set<String> = [], limit: Int = maxCategories) -> [WidgetCategory] {
        let categories: [WidgetCategory] = counts.compactMap { name, counts in
            name.isEmpty ? nil : WidgetCategory(name: name, counts: counts)
        }
        let sorted = categories.sorted { lhs, rhs in
            lhs.counts.total != rhs.counts.total ? lhs.counts.total > rhs.counts.total : lhs.name < rhs.name
        }
        let extra = sorted.filter { required.contains($0.name) }.prefix(maxConfiguredScopes)
        let top = sorted.filter { !required.contains($0.name) }.prefix(max(limit - extra.count, 0))
        return sorted.filter { category in extra.contains(category) || top.contains(category) }
    }

    /// Every filter without a category, then the category scopes of installed widgets.
    static func scopes(configured: [WidgetScope]) -> [WidgetScope] {
        var result = WidgetTorrentFilter.allCases.map { WidgetScope(filter: $0, category: nil) }
        var seen = Set(result)
        var extra = 0
        for scope in configured where scope.category != nil && extra < maxConfiguredScopes && seen.insert(scope).inserted {
            result.append(scope)
            extra += 1
        }
        return result
    }

    /// The union of the top `perGroup` torrents of every scope, capped at
    /// `cap` (indices into `torrents`, ascending).
    static func publishedSubset<T: WidgetRankable>(_ torrents: [T], scopes: [WidgetScope], perGroup: Int = rowsPerGroup,
                                                   cap: Int = maxPublished) -> [Int] {
        guard perGroup > 0, cap > 0, !torrents.isEmpty else { return [] }
        var chosen: [Int] = []
        var seen = Set<Int>()
        for scope in scopes {
            let matching = torrents.indices.filter { matches(torrents[$0], filter: scope.filter, category: scope.category) }
            let best = top(matching, limit: perGroup) { ranks(torrents[$0], before: torrents[$1], filter: scope.filter) }
            for index in best where seen.insert(index).inserted {
                chosen.append(index)
                if chosen.count == cap { return chosen.sorted() }
            }
        }
        return chosen.sorted()
    }

    /// The pinned torrents worth publishing: distinct, valid identifiers, at most `maxPinned`.
    static func pinnedIDs(_ configured: [String]) -> [String] {
        var seen = Set<String>()
        return Array(configured.filter { WidgetText.isTorrentID($0) && seen.insert($0).inserted }.prefix(maxPinned))
    }

    /// The picker's choices: torrents in the order of the "All" filter
    /// (transfers first, then the most recent), at most `limit` (indices).
    static func candidates<T: WidgetRankable>(_ torrents: [T], limit: Int = maxCandidates) -> [Int] {
        let sorted = torrents.indices.sorted { ranks(torrents[$0], before: torrents[$1], filter: .all) }
        return Array(sorted.prefix(max(limit, 0)))
    }

    /// Picker entries whose name contains every word of `text` (case and diacritics ignored).
    static func search(_ candidates: [WidgetTorrentRef], for text: String) -> [WidgetTorrentRef] {
        let words = text.split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return candidates }
        return candidates.filter { candidate in
            words.allSatisfy { candidate.name.range(of: $0, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
        }
    }

    /// The `limit` smallest elements in order, without sorting everything.
    private static func top<T>(_ items: [T], limit: Int, by areInOrder: (T, T) -> Bool) -> [T] {
        guard items.count > limit else { return items.sorted(by: areInOrder) }
        var best: [T] = []
        best.reserveCapacity(limit + 1)
        for item in items {
            if best.count == limit, let last = best.last, !areInOrder(item, last) { continue }
            let position = best.firstIndex { areInOrder(item, $0) } ?? best.endIndex
            best.insert(item, at: position)
            if best.count > limit { best.removeLast() }
        }
        return best
    }
}
