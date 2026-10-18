import SwiftUI
import WidgetKit

/// Starts or stops the widget's torrent; a round icon button, or a
/// full-width one with a title (large widget).
struct TorrentRunButton: View {
    let torrent: WidgetTorrent
    let isEnabled: Bool
    var showsTitle = false

    var body: some View {
        let start = !torrent.state.isRunning
        let title = start ? String(localized: "Start") : String(localized: "Stop")
        Button(intent: SetTorrentRunningIntent(torrentID: torrent.id, start: start)) {
            if showsTitle {
                Label(title, systemImage: start ? "play.fill" : "stop.fill")
                    .font(.system(size: 12, weight: .medium))
                    .frame(maxWidth: .infinity)
            } else {
                Image(systemName: start ? "play.fill" : "stop.fill")
                    .font(.system(size: 11, weight: .bold))
                    .frame(width: 14, height: 14)
            }
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(showsTitle ? .capsule : .circle)
        .tint(start ? WidgetPalette.download : nil)
        .disabled(!isEnabled)
        .accessibilityLabel(Text(title))
    }
}
