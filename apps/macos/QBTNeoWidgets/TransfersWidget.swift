import SwiftUI
import WidgetKit

/// Speeds and the torrents that matter now, filtered by state and category.
struct TransfersWidget: Widget {
    static let kind = WidgetKind.transfers

    var body: some WidgetConfiguration {
        AppIntentConfiguration(kind: Self.kind, intent: TransfersConfigurationIntent.self, provider: TransfersProvider()) { entry in
            TransfersEntryView(entry: entry)
        }
        .configurationDisplayName("Transfers")
        .description("See your download and upload speeds and the torrents that matter right now.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge])
    }
}

/// Reads the family from the environment and adds the container background.
struct TransfersEntryView: View {
    let entry: SnapshotEntry
    @Environment(\.widgetFamily) private var family

    var body: some View {
        TransfersWidgetView(entry: entry, family: family)
            .containerBackground(for: .widget) { WidgetBackground() }
    }
}

#Preview("Small", as: .systemSmall) {
    TransfersWidget()
} timeline: {
    WidgetSampleData.entry()
}

#Preview("Medium", as: .systemMedium) {
    TransfersWidget()
} timeline: {
    WidgetSampleData.entry(filter: .downloading)
}

#Preview("Large", as: .systemLarge) {
    TransfersWidget()
} timeline: {
    WidgetSampleData.entry(filter: .all)
    SnapshotEntry(date: .now, snapshot: WidgetSnapshot(generatedAt: .now, status: .notRunning))
}
