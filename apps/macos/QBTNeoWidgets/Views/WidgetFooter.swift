import SwiftUI

/// The bottom line: a count, or when the data is old, when it was taken.
struct WidgetFooter: View {
    let text: String
    let generatedAt: Date?
    let isStale: Bool

    var body: some View {
        Group {
            if isStale, let generatedAt {
                Label {
                    Text("Updated \(Text(generatedAt, style: .time))")
                } icon: {
                    Image(systemName: "clock.badge.exclamationmark")
                }
            } else {
                Text(text)
            }
        }
        .font(.system(size: 11))
        .foregroundStyle(.secondary)
        .monospacedDigit()
        .lineLimit(1)
        .minimumScaleFactor(0.8)
    }
}
