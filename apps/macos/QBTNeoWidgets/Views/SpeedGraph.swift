import Charts
import SwiftUI
import WidgetKit

/// Download and upload speed over the last half hour: a filled download
/// area and an upload line, without axes (a sparkline at small sizes).
struct SpeedGraph: View {
    let samples: [WidgetSpeedSample]
    var showsUpload = true
    var lineWidth = 1.5

    @Environment(\.widgetRenderingMode) private var renderingMode

    var body: some View {
        Chart {
            ForEach(samples, id: \.time) { sample in
                AreaMark(x: .value("Time", sample.date), y: .value("Download", sample.download), series: .value("Series", "download"))
                    .interpolationMethod(.monotone)
                    .foregroundStyle(areaFill)
                LineMark(x: .value("Time", sample.date), y: .value("Download", sample.download), series: .value("Series", "download"))
                    .interpolationMethod(.monotone)
                    .lineStyle(StrokeStyle(lineWidth: lineWidth, lineCap: .round, lineJoin: .round))
                    .foregroundStyle(WidgetPalette.download)
                if showsUpload {
                    LineMark(x: .value("Time", sample.date), y: .value("Upload", sample.upload), series: .value("Series", "upload"))
                        .interpolationMethod(.monotone)
                        .lineStyle(StrokeStyle(lineWidth: lineWidth, lineCap: .round, lineJoin: .round))
                        .foregroundStyle(WidgetPalette.upload)
                }
            }
        }
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .chartLegend(.hidden)
        .chartYScale(domain: 0...peak)
        .chartXScale(range: .plotDimension(padding: 0))
        .widgetAccentable()
        .accessibilityLabel(Text("Speed in the last 30 minutes"))
    }

    /// Vibrant (desktop in the background) and accented modes get a flat
    /// fill; full color gets a fading gradient.
    private var areaFill: AnyShapeStyle {
        guard renderingMode == .fullColor else { return AnyShapeStyle(WidgetPalette.download.opacity(0.35)) }
        return AnyShapeStyle(LinearGradient(colors: [WidgetPalette.download.opacity(0.45), WidgetPalette.download.opacity(0.03)],
                                            startPoint: .top, endPoint: .bottom))
    }

    private var peak: Int64 {
        let highest = samples.reduce(Int64(0)) { max($0, $1.download, showsUpload ? $1.upload : 0) }
        return max(highest + highest / 8, 1_024)
    }
}
