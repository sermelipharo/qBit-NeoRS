import AppIntents
import WidgetKit

/// The Torrent widget's option: which torrent to follow. Also compiled into
/// the app, which reads installed configurations to know what to pin.
struct TorrentWidgetIntent: WidgetConfigurationIntent {
    static let title: LocalizedStringResource = "Torrent"
    static let description = IntentDescription("Choose the torrent to follow.")
    static let isDiscoverable = false

    @Parameter(title: "Torrent")
    var torrent: WidgetTorrentEntity?

    init() {}

    init(torrent: WidgetTorrentEntity?) {
        self.torrent = torrent
    }
}
