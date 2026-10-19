import Foundation

/// Builds the widgets' snapshot from the app's stores: one pass over the
/// torrents for counts and ranking candidates, then only the torrents of the
/// given scopes are copied (`WidgetTorrentSelector.publishedSubset`).
@MainActor
enum WidgetSnapshotBuilder {
    static func build(transfers: TransferStore, names: TorrentNamesModel?, status: SessionStatus, history: [WidgetSpeedSample], scopes: [WidgetScope],
                      pinned: [String], pinnedHistory: [String: [WidgetSpeedSample]], appStatus: WidgetAppStatus,
                      now: Date = .now) -> WidgetSnapshot {
        var snapshot = WidgetSnapshot(generatedAt: now, status: appStatus)
        guard appStatus == .running || appStatus == .locked else { return snapshot }
        snapshot.downloadRate = status.downloadRate
        snapshot.uploadRate = status.uploadRate
        snapshot.altSpeedEnabled = status.altSpeedEnabled
        snapshot.history = history

        let items = transfers.items
        var candidates: [WidgetCandidate] = []
        candidates.reserveCapacity(items.count)
        var counts = WidgetCounts()
        var categoryCounts: [String: WidgetCounts] = [:]
        for (index, item) in items.enumerated() {
            let candidate = WidgetCandidate(
                index: index, id: item.id, state: item.state.widgetState, progress: item.progress, downloadRate: item.dlSpeed,
                uploadRate: item.upSpeed, category: item.category, addedOn: item.addedOn, completedOn: item.completionOn
            )
            counts.add(candidate)
            if !candidate.category.isEmpty { categoryCounts[candidate.category, default: WidgetCounts()].add(candidate) }
            candidates.append(candidate)
        }
        snapshot.counts = counts
        // A locked UI shows no torrent or category names.
        guard appStatus == .running else { return snapshot }

        snapshot.categories = WidgetTorrentSelector.topCategories(categoryCounts, required: Set(scopes.compactMap(\.category)))
        let published = WidgetTorrentSelector.publishedSubset(candidates, scopes: scopes)
        snapshot.candidates = WidgetTorrentSelector.candidates(candidates).map { WidgetTorrentRef(id: items[$0].id, name: items[$0].name) }
        for id in pinned {
            guard let item = transfers.item(id) else {
                snapshot.removed.append(id)
                continue
            }
            snapshot.pinned.append(detail(item, names: names, history: pinnedHistory[id] ?? []))
        }
        snapshot.torrents = published.map { torrent(items[$0]) }
        return snapshot
    }

    private static func torrent(_ item: TorrentItem) -> WidgetTorrent {
        WidgetTorrent(
            id: item.id, name: item.name, state: item.state.widgetState, progress: item.progress, downloadRate: item.dlSpeed,
            uploadRate: item.upSpeed, eta: item.eta, size: item.size, category: item.category, addedOn: item.addedOn,
            completedOn: item.completionOn
        )
    }

    /// A pinned torrent; its name is split into title and tags as on the cards.
    private static func detail(_ item: TorrentItem, names: TorrentNamesModel?, history: [WidgetSpeedSample]) -> WidgetTorrentDetail {
        let parsed = names?.parsed(item.name)
        let chips = names?.showsCardTags == true ? parsed?.chips ?? [] : []
        return WidgetTorrentDetail(
            torrent: torrent(item), downloaded: item.completed, ratio: item.ratio, seeds: item.numSeeds, seedsInSwarm: item.numComplete,
            peers: item.numLeechs, peersInSwarm: item.numIncomplete, availability: item.availability, tags: item.tags,
            savePath: item.savePath, errorMessage: item.errorMessage, history: history, title: parsed?.title,
            chips: chips.prefix(WidgetTorrentDetail.maxChips).map {
                WidgetNameChip(text: $0.text, tint: $0.category.chipTint, isOutlined: NameChipStyle.isOutlined($0.category))
            }
        )
    }
}
