import SwiftUI
import WidgetKit

/// The Speed & Controls widget: speeds with a graph, and buttons for
/// alternative speed limits (small) plus starting and stopping every
/// torrent (medium).
struct SpeedWidgetView: View {
    let entry: SnapshotEntry
    let family: WidgetFamily

    var body: some View {
        if let notice = entry.notice {
            WidgetNotice(content: notice, compact: family == .systemSmall)
        } else if let snapshot = entry.snapshot {
            switch family {
            case .systemSmall:
                VStack(spacing: 6) {
                    SpeedSummary(entry: entry, snapshot: snapshot, title: String(localized: "Speed"),
                                 footer: WidgetTorrentFilter.active.countText(snapshot.counts), graphHeight: 22)
                    altSpeedButton(snapshot, short: true)
                }
            default:
                HStack(spacing: 14) {
                    SpeedSummary(entry: entry, snapshot: snapshot, title: String(localized: "Speed"),
                                 footer: WidgetTorrentFilter.active.countText(snapshot.counts), graphHeight: 44)
                    VStack(spacing: 6) {
                        ControlButton(title: String(localized: "Start All"), symbol: "play.fill", intent: StartAllIntent(),
                                      isEnabled: entry.acceptsCommands)
                        ControlButton(title: String(localized: "Stop All"), symbol: "stop.fill", intent: StopAllIntent(),
                                      isEnabled: entry.acceptsCommands)
                        altSpeedButton(snapshot, short: false)
                    }
                    .frame(width: 132)
                }
            }
        }
    }

    private func altSpeedButton(_ snapshot: WidgetSnapshot, short: Bool) -> some View {
        let isOn = snapshot.altSpeedEnabled
        let title = short ? String(localized: "Alt. Limits") : String(localized: "Alt. Speed Limits")
        return ControlButton(title: title, symbol: isOn ? "tortoise.fill" : "tortoise", intent: SetAltSpeedIntent(enabled: !isOn),
                             isOn: isOn, tint: WidgetPalette.altSpeed, isEnabled: entry.acceptsCommands)
            .accessibilityValue(Text(isOn ? String(localized: "On") : String(localized: "Off")))
    }
}
