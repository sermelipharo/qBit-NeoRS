import SwiftUI
import WidgetKit

/// Speeds with a graph, and buttons that start or stop every torrent and
/// switch alternative speed limits.
struct SpeedWidget: Widget {
    static let kind = WidgetKind.speed

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: Self.kind, provider: SpeedProvider()) { entry in
            SpeedEntryView(entry: entry)
        }
        .configurationDisplayName("Speed & Controls")
        .description("Watch transfer speeds and start, stop or slow down all torrents at once.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

/// Reads the family from the environment and adds the container background.
struct SpeedEntryView: View {
    let entry: SnapshotEntry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        SpeedWidgetView(entry: entry, family: family)
            .containerBackground(for: .widget) { WidgetBackground() }
    }
}

#Preview("Small", as: .systemSmall) {
    SpeedWidget()
} timeline: {
    WidgetSampleData.entry()
}

#Preview("Medium", as: .systemMedium) {
    SpeedWidget()
} timeline: {
    WidgetSampleData.entry()
    SnapshotEntry(date: .now, snapshot: WidgetSampleData.snapshot(altSpeed: true))
}
