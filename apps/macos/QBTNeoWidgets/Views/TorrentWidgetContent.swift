import Foundation

/// What a Torrent widget shows for its entry.
enum TorrentWidgetContent {
    case notice(WidgetNoticeContent)
    /// The app has not published the chosen torrent yet (just configured).
    case waiting(name: String?)
    /// The torrent is no longer in the app.
    case removed(name: String?)
    case detail(WidgetTorrentDetail)

    init(entry: SnapshotEntry) {
        guard let id = entry.pinnedID else {
            self = .notice(WidgetNoticeContent(symbol: "hand.point.up.left", title: String(localized: "Choose a torrent"),
                                               message: String(localized: "Edit the widget to pick the torrent to follow.")))
            return
        }
        if let notice = entry.notice {
            self = .notice(notice)
        } else if entry.snapshot?.status == .locked {
            self = .notice(WidgetNoticeContent(symbol: "lock.fill", title: String(localized: "qBit NeoRS is locked"), message: ""))
        } else if let detail = entry.snapshot?.pinned(id) {
            self = .detail(detail)
        } else if entry.snapshot?.removed.contains(id) == true {
            self = .removed(name: entry.pinnedName)
        } else {
            self = .waiting(name: entry.pinnedName)
        }
    }
}
