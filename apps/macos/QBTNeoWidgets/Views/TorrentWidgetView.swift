import SwiftUI
import WidgetKit

/// The Torrent widget: one chosen torrent. Small shows the ring, state and
/// name (its title, with the technical tags that fit below, as on the cards); medium adds speeds, size, swarm and its own speed graph; large adds
/// every detail and a Start/Stop button with a title.
struct TorrentWidgetView: View {
    let entry: SnapshotEntry
    let family: WidgetFamily

    var body: some View {
        switch TorrentWidgetContent(entry: entry) {
        case .notice(let notice):
            WidgetNotice(content: notice, compact: family == .systemSmall)
        case .waiting(let name):
            WidgetNotice(content: WidgetNoticeContent(symbol: "hourglass", title: name ?? String(localized: "Torrent"),
                                                      message: String(localized: "Waiting for qBit NeoRS…")),
                         compact: family == .systemSmall)
        case .removed(let name):
            WidgetNotice(content: WidgetNoticeContent(symbol: "trash", title: String(localized: "No longer in qBit NeoRS"),
                                                      message: name ?? ""),
                         compact: family == .systemSmall)
        case .detail(let detail):
            content(detail)
                .opacity(entry.isStale ? 0.6 : 1)
                .widgetURL(WidgetLink.torrent(detail.id))
        }
    }

    @ViewBuilder
    private func content(_ detail: WidgetTorrentDetail) -> some View {
        switch family {
        case .systemSmall: small(detail)
        case .systemMedium: medium(detail)
        default: large(detail)
        }
    }

    // MARK: Layouts

    private func small(_ detail: WidgetTorrentDetail) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top) {
                TorrentRing(detail: detail, diameter: 58)
                Spacer(minLength: 4)
                TorrentRunButton(torrent: detail.torrent, isEnabled: entry.acceptsCommands)
            }
            Spacer(minLength: 0)
            Text(detail.displayTitle)
                .font(.system(size: 12, weight: .semibold))
                .lineLimit(chips(detail).isEmpty ? 2 : 1)
            chipRow(detail)
            compactStatus(detail)
        }
    }

    private func medium(_ detail: WidgetTorrentDetail) -> some View {
        HStack(spacing: 14) {
            TorrentRing(detail: detail, diameter: 92)
            VStack(alignment: .leading, spacing: 3) {
                HStack(alignment: .top, spacing: 6) {
                    Text(detail.displayTitle)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(chips(detail).isEmpty ? 2 : 1)
                    Spacer(minLength: 0)
                    TorrentRunButton(torrent: detail.torrent, isEnabled: entry.acceptsCommands)
                }
                chipRow(detail)
                statusLine(detail)
                Spacer(minLength: 2)
                secondary(TorrentDetailText.size(detail))
                secondary(TorrentDetailText.swarm(detail))
                SpeedGraph(samples: detail.history, lineWidth: 1.25)
                    .frame(height: 22)
            }
        }
    }

    private func large(_ detail: WidgetTorrentDetail) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 14) {
                TorrentRing(detail: detail, diameter: 96)
                VStack(alignment: .leading, spacing: 4) {
                    Text(detail.displayTitle)
                        .font(.system(size: 15, weight: .semibold))
                        .lineLimit(3)
                        .fixedSize(horizontal: false, vertical: true)
                    chipRow(detail)
                    statusLine(detail)
                    let labels = TorrentDetailText.labels(detail)
                    if !labels.isEmpty { secondary(labels) }
                }
            }
            HStack(alignment: .firstTextBaseline) {
                SpeedReadout(direction: .download, rate: detail.torrent.downloadRate, numberSize: 20)
                Spacer(minLength: 8)
                SpeedReadout(direction: .upload, rate: detail.torrent.uploadRate, numberSize: 20)
            }
            SpeedGraph(samples: detail.history, lineWidth: 1.5)
                .frame(height: 48)
            Grid(alignment: .leading, horizontalSpacing: 10, verticalSpacing: 4) {
                GridRow {
                    stat(String(localized: "Size"), TorrentDetailText.size(detail))
                    stat(String(localized: "Ratio"), TorrentDetailText.ratio(detail))
                }
                GridRow {
                    stat(String(localized: "Seeds"), TorrentDetailText.connections(detail.seeds, detail.seedsInSwarm))
                    stat(String(localized: "Peers"), TorrentDetailText.connections(detail.peers, detail.peersInSwarm))
                }
                GridRow {
                    stat(String(localized: "Availability"), TorrentDetailText.availability(detail))
                    stat(String(localized: "Location"), detail.location.isEmpty ? "–" : detail.location)
                }
            }
            Spacer(minLength: 0)
            TorrentRunButton(torrent: detail.torrent, isEnabled: entry.acceptsCommands, showsTitle: true)
        }
    }

    // MARK: Parts

    private func chips(_ detail: WidgetTorrentDetail) -> [WidgetNameChip] {
        detail.chips ?? []
    }

    /// The name's technical tags that fit on one line, as on the cards.
    @ViewBuilder
    private func chipRow(_ detail: WidgetTorrentDetail) -> some View {
        let chips = chips(detail)
        if !chips.isEmpty {
            NameChipRow(chips: chips)
                .padding(.vertical, 1)
        }
    }

    /// The state in its color, then what it is doing (or when it finished).
    private func statusLine(_ detail: WidgetTorrentDetail) -> some View {
        let torrent = detail.torrent
        return HStack(spacing: 4) {
            Text(torrent.state.title)
                .foregroundStyle(torrent.state.color)
                .fontWeight(.semibold)
                .widgetAccentable()
            if entry.isStale {
                Text("· Updated \(Text(entry.snapshot?.generatedAt ?? entry.date, style: .time))")
                    .foregroundStyle(.secondary)
            } else if torrent.state == .completed, let date = TorrentDetailText.completedDate(detail) {
                Text("· \(Text(date, format: .relative(presentation: .named)))")
                    .foregroundStyle(.secondary)
            } else if torrent.state != .stopped, torrent.state != .completed {
                let activity = TorrentDetailText.activity(detail)
                if activity != torrent.state.title {
                    Text("· " + activity)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .font(.system(size: 11))
        .monospacedDigit()
        .lineLimit(1)
        .minimumScaleFactor(0.85)
    }

    /// Small widget: what a transfer is doing (the ring shows the state), else the state.
    @ViewBuilder
    private func compactStatus(_ detail: WidgetTorrentDetail) -> some View {
        let torrent = detail.torrent
        let activity = TorrentDetailText.activity(detail)
        let isTransfer = torrent.state.isDownloadingState || torrent.state.isSeedingState
        if isTransfer, activity != torrent.state.title, !entry.isStale {
            secondary(activity)
        } else {
            statusLine(detail)
        }
    }

    private func secondary(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 11))
            .foregroundStyle(.secondary)
            .monospacedDigit()
            .lineLimit(1)
            .minimumScaleFactor(0.85)
    }

    private func stat(_ title: String, _ value: String) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(title)
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
            Text(value)
                .font(.system(size: 12, weight: .medium))
                .monospacedDigit()
                .lineLimit(1)
                .truncationMode(.head)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
