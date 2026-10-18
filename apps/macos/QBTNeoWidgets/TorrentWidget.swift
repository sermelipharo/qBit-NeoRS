import SwiftUI
import WidgetKit

/// Follows one chosen torrent: progress, speeds and a Start/Stop button.
struct TorrentWidget: Widget {
    static let kind = WidgetKind.torrent

    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: Self.kind, intent: TorrentWidgetIntent.self, provider: TorrentProvider()) { entry in
            TorrentEntryView(entry: entry)
        }
        .configurationDisplayName("Torrent")
        .description("Follow one torrent's progress and speed, and start or stop it.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
    }
}

/// Reads the family from the environment and adds the container background.
struct TorrentEntryView: View {
    let entry: SnapshotEntry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        TorrentWidgetView(entry: entry, family: family)
            .containerBackground(for: .widget) { WidgetBackground() }
    }
}

#Preview("Small", as: .systemSmall) {
    TorrentWidget()
} timeline: {
    WidgetSampleData.torrentEntry(.downloading)
    WidgetSampleData.torrentEntry(.completed)
}

#Preview("Medium", as: .systemMedium) {
    TorrentWidget()
} timeline: {
    WidgetSampleData.torrentEntry(.downloading)
    WidgetSampleData.torrentEntry(.missingFiles)
}

#Preview("Large", as: .systemLarge) {
    TorrentWidget()
} timeline: {
    WidgetSampleData.torrentEntry(.seeding)
}
