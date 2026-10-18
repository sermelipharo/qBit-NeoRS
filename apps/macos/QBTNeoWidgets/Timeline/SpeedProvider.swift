import WidgetKit

/// Timelines of the Speed & Controls widget (no options).
struct SpeedProvider: TimelineProvider {
    func placeholder(in context: Context) -> SnapshotEntry {
        WidgetSampleData.entry()
    }

    func getSnapshot(in context: Context, completion: @escaping @Sendable (SnapshotEntry) -> Void) {
        completion(SnapshotTimeline.currentEntry(isPreview: context.isPreview, template: SnapshotEntry(date: .now)))
    }

    func getTimeline(in context: Context, completion: @escaping @Sendable (Timeline<SnapshotEntry>) -> Void) {
        completion(SnapshotTimeline.timeline(template: SnapshotEntry(date: .now)))
    }
}
