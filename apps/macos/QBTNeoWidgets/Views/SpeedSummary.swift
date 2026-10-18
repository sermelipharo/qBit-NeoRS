import SwiftUI

/// Header, both speeds, a sparkline and a footer: the small widgets, and the
/// left column of the medium ones.
struct SpeedSummary: View {
    let entry: SnapshotEntry
    let snapshot: WidgetSnapshot
    var title: String
    var footer: String
    var graphHeight = 30.0

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            WidgetHeader(title: title, altSpeedEnabled: snapshot.altSpeedEnabled)
            Spacer(minLength: 4)
            SpeedReadout(direction: .download, rate: snapshot.downloadRate, numberSize: 24)
            SpeedReadout(direction: .upload, rate: snapshot.uploadRate, numberSize: 16)
            Spacer(minLength: 4)
            SpeedGraph(samples: snapshot.history)
                .frame(height: graphHeight)
            Spacer(minLength: 4)
            WidgetFooter(text: footer, generatedAt: snapshot.generatedAt, isStale: entry.isStale)
        }
        .opacity(entry.isStale ? 0.6 : 1)
    }
}
