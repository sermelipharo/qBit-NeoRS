import SwiftUI
import WidgetKit

/// The Transfers widget: speeds and the torrents matching its filter.
/// Small shows speeds and a count, medium adds the three most relevant
/// torrents, large a bigger graph and five torrents.
struct TransfersWidgetView: View {
    let entry: SnapshotEntry
    let family: WidgetFamily

    var body: some View {
        if let notice = entry.notice {
            WidgetNotice(content: notice, compact: family == .systemSmall)
        } else if let snapshot = entry.snapshot {
            content(snapshot)
        }
    }

    @ViewBuilder
    private func content(_ snapshot: WidgetSnapshot) -> some View {
        let footer = entry.filter.countText(snapshot.counts(category: entry.category))
        switch family {
        case .systemSmall:
            SpeedSummary(entry: entry, snapshot: snapshot, title: entry.scopeTitle, footer: footer)
        case .systemMedium:
            HStack(spacing: 14) {
                SpeedSummary(entry: entry, snapshot: snapshot, title: entry.scopeTitle, footer: footer)
                    .frame(width: 128)
                TorrentList(entry: entry, snapshot: snapshot, limit: 3, spacing: 9)
                    .opacity(entry.isStale ? 0.6 : 1)
            }
        default:
            large(snapshot, footer: footer)
        }
    }

    private func large(_ snapshot: WidgetSnapshot, footer: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            WidgetHeader(title: entry.scopeTitle, altSpeedEnabled: snapshot.altSpeedEnabled)
            HStack(alignment: .firstTextBaseline) {
                SpeedReadout(direction: .download, rate: snapshot.downloadRate, numberSize: 28)
                Spacer(minLength: 8)
                SpeedReadout(direction: .upload, rate: snapshot.uploadRate, numberSize: 20)
            }
            SpeedGraph(samples: snapshot.history, lineWidth: 1.75)
                .frame(height: 58)
            Divider()
            TorrentList(entry: entry, snapshot: snapshot, limit: 5, spacing: 7)
            WidgetFooter(text: footer, generatedAt: snapshot.generatedAt, isStale: entry.isStale)
        }
        .opacity(entry.isStale ? 0.6 : 1)
    }
}
