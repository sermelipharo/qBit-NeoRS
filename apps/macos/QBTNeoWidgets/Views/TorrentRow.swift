import SwiftUI

/// One torrent: progress ring, name, and a status line that fits its state.
struct TorrentRow: View {
    let torrent: WidgetTorrent
    var ringDiameter = 26.0

    var body: some View {
        HStack(spacing: 8) {
            ProgressRing(progress: torrent.progress, state: torrent.state, diameter: ringDiameter)
            VStack(alignment: .leading, spacing: 1) {
                Text(torrent.name)
                    .font(.system(size: 12, weight: .medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
                Text(detail)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: "\(torrent.name), \(detail)"))
    }

    private var detail: String {
        let percent = WidgetFormat.percent(torrent.progress)
        switch torrent.state {
        case .downloading, .stalledDownloading, .metadata:
            var parts = [percent]
            if torrent.downloadRate > 0 { parts.append("↓ " + WidgetFormat.speed(torrent.downloadRate)) }
            if let eta = torrent.remainingTime {
                parts.append(WidgetFormat.eta(eta))
            } else if torrent.downloadRate == 0 {
                parts.append(torrent.state.title)
            }
            return parts.joined(separator: " · ")
        case .seeding, .stalledSeeding:
            guard torrent.uploadRate > 0 else { return torrent.state.title }
            return torrent.state.title + " · ↑ " + WidgetFormat.speed(torrent.uploadRate)
        case .completed, .error, .missingFiles:
            return torrent.state.title
        case .queued, .checking, .moving, .stopped:
            return torrent.progress < 1 ? "\(percent) · \(torrent.state.title)" : torrent.state.title
        }
    }
}
