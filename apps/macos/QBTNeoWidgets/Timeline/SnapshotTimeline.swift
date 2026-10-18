import Foundation
import WidgetKit

/// Turns the app's snapshot into timeline entries. Besides the current entry
/// a second one, dated when the app's heartbeat is overdue, marks the data as
/// stale, so a crashed or hung app never leaves old speeds looking live.
enum SnapshotTimeline {
    /// Longer than the app's 15-minute heartbeat (`WidgetPublishPolicy`).
    static let staleAfter: TimeInterval = 20 * 60
    /// When WidgetKit should ask again if the app sends no reload.
    static let refreshAfter: TimeInterval = 15 * 60

    /// `template` carries the widget's configuration (filter, category, pinned torrent).
    static func entries(snapshot: WidgetSnapshot?, now: Date, template: SnapshotEntry) -> [SnapshotEntry] {
        var current = template
        current.date = now
        current.snapshot = snapshot
        current.isStale = false
        guard let snapshot, snapshot.status == .running || snapshot.status == .locked else { return [current] }
        let staleDate = snapshot.generatedAt.addingTimeInterval(staleAfter)
        var stale = current
        stale.isStale = true
        guard staleDate > now else { return [stale] }
        stale.date = staleDate
        return [current, stale]
    }

    static func timeline(template: SnapshotEntry, now: Date = .now) -> Timeline<SnapshotEntry> {
        // Tells a running app that a widget exists or changed (it then
        // starts publishing, or re-reads the widgets' configurations).
        WidgetMessage.timelineRequested.post()
        let entries = entries(snapshot: WidgetSnapshotStore.load(), now: now, template: template)
        return Timeline(entries: entries, policy: .after(now.addingTimeInterval(refreshAfter)))
    }

    /// The gallery shows sample data; an installed widget its real data.
    static func currentEntry(isPreview: Bool, template: SnapshotEntry, now: Date = .now) -> SnapshotEntry {
        guard !isPreview else {
            var sample = WidgetSampleData.entry(filter: template.filter, category: template.category, now: now)
            sample.pinnedID = template.pinnedID == nil ? nil : WidgetSampleData.pinnedID
            sample.pinnedName = template.pinnedName
            return sample
        }
        return entries(snapshot: WidgetSnapshotStore.load(), now: now, template: template)[0]
    }
}
