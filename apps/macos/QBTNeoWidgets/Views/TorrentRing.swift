import SwiftUI
import WidgetKit

/// The Torrent widget's large progress ring. The center shows the percentage
/// while downloading, a check when complete, an arrow while seeding and an
/// exclamation mark when the torrent needs attention. A completed download
/// gets a soft purple-to-pink ring in full color.
struct TorrentRing: View {
    let detail: WidgetTorrentDetail
    var diameter = 64.0

    @Environment(\.widgetRenderingMode) private var renderingMode

    private var torrent: WidgetTorrent { detail.torrent }

    var body: some View {
        let lineWidth = max(diameter * 0.1, 5)
        ZStack {
            Circle()
                .stroke(.quaternary, lineWidth: lineWidth)
            Circle()
                .trim(from: 0, to: max(torrent.progress, 0.002))
                .stroke(ringStyle, style: StrokeStyle(lineWidth: lineWidth, lineCap: .round))
                .rotationEffect(.degrees(-90))
                .widgetAccentable()
            center
        }
        .padding(lineWidth / 2)
        .frame(width: diameter, height: diameter)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(torrent.state.title))
        .accessibilityValue(Text(WidgetFormat.percent(torrent.progress)))
    }

    @ViewBuilder
    private var center: some View {
        if let symbol = centerSymbol {
            Image(systemName: symbol)
                .font(.system(size: diameter * 0.3, weight: .bold))
                .foregroundStyle(torrent.state.color)
                .widgetAccentable()
        } else {
            Text(WidgetFormat.percent(torrent.progress))
                .font(.system(size: diameter * 0.24, weight: .semibold, design: .rounded))
                .monospacedDigit()
                .minimumScaleFactor(0.6)
                .lineLimit(1)
                .padding(.horizontal, diameter * 0.12)
                .contentTransition(.numericText(value: torrent.progress))
        }
    }

    private var centerSymbol: String? {
        switch torrent.state {
        case .completed: "checkmark"
        case .seeding, .stalledSeeding: "arrow.up"
        case .error, .missingFiles: "exclamationmark"
        default: nil
        }
    }

    private var ringStyle: AnyShapeStyle {
        guard torrent.state == .completed, renderingMode == .fullColor else { return AnyShapeStyle(torrent.state.color) }
        return AnyShapeStyle(AngularGradient(colors: [.purple, .pink, .purple], center: .center, startAngle: .degrees(-90),
                                             endAngle: .degrees(270)))
    }
}
