import SwiftUI
import WidgetKit

/// A torrent's progress as a ring around its state symbol.
struct ProgressRing: View {
    let progress: Double
    let state: WidgetTorrentState
    var diameter = 26.0

    var body: some View {
        let lineWidth = diameter * 0.13
        ZStack {
            Circle()
                .stroke(.quaternary, lineWidth: lineWidth)
            Circle()
                .trim(from: 0, to: max(progress, 0.001))
                .stroke(state.color, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
                .widgetAccentable()
            Image(systemName: state.systemImage)
                .font(.system(size: diameter * 0.36, weight: .bold))
                .foregroundStyle(state.color)
                .widgetAccentable()
        }
        .padding(lineWidth / 2)
        .frame(width: diameter, height: diameter)
        .accessibilityHidden(true)
    }
}
