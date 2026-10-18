import WidgetKit

/// Timelines of the Torrent widget, which follows one chosen torrent.
struct TorrentProvider: AppIntentTimelineProvider {
    func placeholder(in context: Context) -> SnapshotEntry {
        var entry = WidgetSampleData.entry()
        entry.pinnedID = WidgetSampleData.pinnedID
        return entry
    }

    func snapshot(for configuration: TorrentWidgetIntent, in context: Context) async -> SnapshotEntry {
        // The gallery shows the sample torrent before one is chosen.
        var template = template(configuration)
        if context.isPreview, template.pinnedID == nil { template.pinnedID = WidgetSampleData.pinnedID }
        return SnapshotTimeline.currentEntry(isPreview: context.isPreview, template: template)
    }

    func timeline(for configuration: TorrentWidgetIntent, in context: Context) async -> Timeline<SnapshotEntry> {
        SnapshotTimeline.timeline(template: template(configuration))
    }

    private func template(_ configuration: TorrentWidgetIntent) -> SnapshotEntry {
        SnapshotEntry(date: .now, pinnedID: configuration.torrent?.id, pinnedName: configuration.torrent?.name)
    }
}
