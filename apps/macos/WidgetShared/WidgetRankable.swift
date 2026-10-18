import Foundation

/// What the widget selector needs to filter and rank a torrent. The app ranks
/// lightweight candidates (no names copied) and the widget ranks the
/// published `WidgetTorrent`s with the same rules, so both agree on the top
/// entries of every filter.
protocol WidgetRankable {
    /// Unique and stable; the final tie-breaker, so rankings are total.
    var id: String { get }
    var state: WidgetTorrentState { get }
    var progress: Double { get }
    var downloadRate: Int64 { get }
    var uploadRate: Int64 { get }
    var category: String { get }
    /// Unix seconds (0 or less when unknown).
    var addedOn: Int64 { get }
    /// Unix seconds (0 or less while incomplete).
    var completedOn: Int64 { get }
}

extension WidgetRankable {
    var isTransferring: Bool { downloadRate > 0 || uploadRate > 0 }

    var totalRate: Int64 { downloadRate &+ uploadRate }

    /// The most recent of "added" and "completed".
    var lastEvent: Int64 { max(addedOn, completedOn) }

    /// Fetching data, or incomplete and waiting in the queue, checking or moving.
    var isDownloading: Bool {
        state.isDownloadingState || (state.isPending && progress < 1)
    }
}
