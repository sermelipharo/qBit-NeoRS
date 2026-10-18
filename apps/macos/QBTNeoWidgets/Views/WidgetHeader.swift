import SwiftUI
import WidgetKit

/// The top line: what the widget shows, and a tortoise while alternative
/// speed limits are on.
struct WidgetHeader: View {
    let title: String
    let altSpeedEnabled: Bool

    var body: some View {
        HStack(spacing: 4) {
            Text(title)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Spacer(minLength: 0)
            if altSpeedEnabled {
                Image(systemName: "tortoise.fill")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(WidgetPalette.altSpeed)
                    .widgetAccentable()
                    .accessibilityLabel(Text("Alternative speed limits are on"))
            }
        }
    }
}
