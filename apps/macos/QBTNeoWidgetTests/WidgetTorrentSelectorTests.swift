import Foundation
import Testing

struct WidgetTorrentSelectorTests {
    private func torrent(_ id: String, _ state: WidgetTorrentState, down: Int64 = 0, up: Int64 = 0, progress: Double = 0.5,
                         category: String = "", added: Int64 = 0, completed: Int64 = 0) -> WidgetTorrent {
        WidgetTorrent(id: id, name: id, state: state, progress: progress, downloadRate: down, uploadRate: up, eta: -1, size: 0,
                      category: category, addedOn: added, completedOn: completed)
    }

    @Test func activeListsTransferringTorrentsFastestFirst() {
        let torrents = [
            torrent("a", .downloading, down: 100), torrent("b", .seeding, up: 500), torrent("c", .stalledDownloading),
            torrent("d", .downloading, down: 300, up: 300),
        ]
        let ids = WidgetTorrentSelector.select(torrents, filter: .active, category: nil, limit: 10).map(\.id)
        #expect(ids == ["d", "b", "a"])
    }

    @Test func downloadingIncludesPendingIncompleteTorrentsOnly() {
        let torrents = [
            torrent("queued", .queued, progress: 0.2), torrent("queuedSeed", .queued, progress: 1), torrent("dl", .downloading, down: 5),
            torrent("stalled", .stalledDownloading), torrent("stopped", .stopped), torrent("meta", .metadata),
        ]
        let ids = WidgetTorrentSelector.select(torrents, filter: .downloading, category: nil, limit: 10).map(\.id)
        #expect(ids == ["dl", "meta", "stalled", "queued"])
    }

    @Test func seedingSortsByUploadRate() {
        let torrents = [torrent("a", .stalledSeeding), torrent("b", .seeding, up: 10), torrent("c", .completed)]
        #expect(WidgetTorrentSelector.select(torrents, filter: .seeding, category: nil, limit: 10).map(\.id) == ["b", "a"])
    }

    @Test func recentUsesTheLatestOfAddedAndCompleted() {
        let torrents = [torrent("old", .completed, added: 10, completed: 50), torrent("new", .stopped, added: 40), torrent("mid", .stopped, added: 30)]
        #expect(WidgetTorrentSelector.select(torrents, filter: .recent, category: nil, limit: 2).map(\.id) == ["old", "new"])
    }

    @Test func categoryAndLimitApply() {
        let torrents = (0..<10).map { torrent("t\($0)", .downloading, down: Int64($0), category: $0.isMultiple(of: 2) ? "even" : "odd") }
        let ids = WidgetTorrentSelector.select(torrents, filter: .all, category: "even", limit: 3).map(\.id)
        #expect(ids == ["t8", "t6", "t4"])
        #expect(WidgetTorrentSelector.select(torrents, filter: .all, category: nil, limit: 0).isEmpty)
    }

    @Test func topCategoriesSortByCountThenName() {
        let counts = ["b": WidgetCounts(total: 2), "a": WidgetCounts(total: 2), "c": WidgetCounts(total: 5), "": WidgetCounts(total: 9)]
        #expect(WidgetTorrentSelector.topCategories(counts, limit: 2).map(\.name) == ["c", "a"])
    }

    /// The widget, selecting from the published subset, must show exactly
    /// what it would show with every torrent.
    @Test(arguments: [1, 2, 3, 4, 5] as [UInt64])
    func publishedSubsetPreservesEverySelection(seed: UInt64) {
        let torrents = TestTorrents.make(count: 400, seed: seed)
        let categories = Array(TestTorrents.categories.dropFirst())
        let configured = categories.flatMap { category in
            WidgetTorrentFilter.allCases.map { WidgetScope(filter: $0, category: category) }
        }
        let scopes = WidgetTorrentSelector.scopes(configured: configured)
        let indices = WidgetTorrentSelector.publishedSubset(torrents, scopes: scopes)
        #expect(indices.count <= WidgetTorrentSelector.maxPublished)
        let subset = indices.map { torrents[$0] }
        for scope in scopes {
            for limit in [3, 5, WidgetTorrentSelector.rowsPerGroup] {
                let full = WidgetTorrentSelector.select(torrents, filter: scope.filter, category: scope.category, limit: limit)
                let published = WidgetTorrentSelector.select(subset, filter: scope.filter, category: scope.category, limit: limit)
                #expect(full == published, "\(scope), limit \(limit)")
            }
        }
    }

    @Test func publishedSubsetRespectsTheCap() {
        let torrents = TestTorrents.make(count: 300, seed: 9)
        let scopes = WidgetTorrentSelector.scopes(configured: [WidgetScope(filter: .all, category: "Linux")])
        let subset = WidgetTorrentSelector.publishedSubset(torrents, scopes: scopes, perGroup: 6, cap: 10)
        #expect(subset.count == 10)
        #expect(subset == subset.sorted())
        #expect(WidgetTorrentSelector.publishedSubset([WidgetTorrent](), scopes: scopes).isEmpty)
    }

    /// The cap covers the worst case: every overall group plus the maximum
    /// number of configured scopes, with no torrent shared between groups.
    @Test func capCoversEveryPublishedScope() {
        let configured = (0..<20).map { WidgetScope(filter: .all, category: "c\($0)") }
        let scopes = WidgetTorrentSelector.scopes(configured: configured + configured)
        #expect(scopes.count == WidgetTorrentFilter.allCases.count + WidgetTorrentSelector.maxConfiguredScopes)
        #expect(scopes.count * WidgetTorrentSelector.rowsPerGroup <= WidgetTorrentSelector.maxPublished)
        #expect(WidgetTorrentSelector.scopes(configured: [WidgetScope(filter: .seeding, category: nil)]).count
            == WidgetTorrentFilter.allCases.count)
    }

    @Test func requiredCategoriesAreAlwaysOffered() {
        var counts: [String: WidgetCounts] = [:]
        for index in 0..<30 { counts["c\(index)"] = WidgetCounts(total: 100 - index) }
        let categories = WidgetTorrentSelector.topCategories(counts, required: ["c29"], limit: 4).map(\.name)
        #expect(categories == ["c0", "c1", "c2", "c29"])
    }
}
