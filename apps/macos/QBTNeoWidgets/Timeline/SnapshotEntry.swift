import Foundation
import WidgetKit

/// A timeline entry: the app's last snapshot plus the widget's configuration.
struct SnapshotEntry: TimelineEntry {
    var date: Date
    /// Nil until the app has published anything.
    var snapshot: WidgetSnapshot?
    /// The app has not refreshed the snapshot for longer than its heartbeat.
    var isStale = false
    var filter: WidgetTorrentFilter = .active
    var category: String?
    /// The torrent a Torrent widget follows (its configuration).
    var pinnedID: String?
    /// The torrent's name as the configuration remembers it.
    var pinnedName: String?

    /// The widget's title: the category when one is chosen, else the filter.
    var scopeTitle: String { category ?? filter.title }

    /// A message replacing the content when there is nothing current to show.
    var notice: WidgetNoticeContent? {
        guard let snapshot else {
            return WidgetNoticeContent(symbol: "arrow.down.circle", title: String(localized: "qBit NeoRS"),
                                       message: String(localized: "Open qBit NeoRS to see your transfers."))
        }
        switch snapshot.status {
        case .running, .locked:
            return nil
        case .notRunning:
            return WidgetNoticeContent(symbol: "moon.zzz", title: String(localized: "qBit NeoRS isn’t running"),
                                       message: String(localized: "Open it to see your transfers."))
        case .updatesOff:
            return WidgetNoticeContent(symbol: "pause.circle", title: String(localized: "Widget updates are off"),
                                       message: String(localized: "Turn on “Update widgets” in qBit NeoRS Settings › General."))
        }
    }

    /// Whether the buttons can reach a running, unlocked app.
    var acceptsCommands: Bool { snapshot?.acceptsCommands == true && !isStale }
}
