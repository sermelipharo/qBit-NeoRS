import WidgetKit

/// Timelines of the configurable Transfers widget.
struct TransfersProvider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> SnapshotEntry {
        WidgetSampleData.entry()
    }

    func snapshot(for configuration: TransfersConfigurationIntent, in context: Context) async -> SnapshotEntry {
        SnapshotTimeline.currentEntry(isPreview: context.isPreview, template: template(configuration))
    }

    func timeline(for configuration: TransfersConfigurationIntent, in context: Context) async -> Timeline<SnapshotEntry> {
        SnapshotTimeline.timeline(template: template(configuration))
    }

    private func template(_ configuration: TransfersConfigurationIntent) -> SnapshotEntry {
        SnapshotEntry(date: .now, filter: configuration.filter.filter, category: configuration.category?.id)
    }
}
