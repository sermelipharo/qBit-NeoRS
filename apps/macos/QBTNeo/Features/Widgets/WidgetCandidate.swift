import Foundation

/// A torrent reduced to what ranking needs, so building a snapshot copies no
/// names for torrents that are not published.
struct WidgetCandidate: WidgetRankable {
    /// Position in `TransferStore.items`.
    let index: Int
    let id: String
    let state: WidgetTorrentState
    let progress: Double
    let downloadRate: Int64
    let uploadRate: Int64
    let category: String
    let addedOn: Int64
    let completedOn: Int64
}
