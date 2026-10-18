import AppIntents
import SwiftUI
import WidgetKit

/// A full-width button that runs an intent, tinted when `isOn`.
struct ControlButton<Intent: AppIntent>: View {
    let title: String
    let symbol: String
    let intent: Intent
    var isOn = false
    var tint: Color = .accentColor
    var isEnabled = true

    var body: some View {
        Button(intent: intent) {
            Label {
                Text(title)
                    .lineLimit(1)
                    .minimumScaleFactor(0.75)
            } icon: {
                Image(systemName: symbol)
                    .foregroundStyle(isOn ? tint : .secondary)
                    .widgetAccentable()
            }
            .font(.system(size: 12, weight: .medium))
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.bordered)
        .buttonBorderShape(.capsule)
        .tint(isOn ? tint : nil)
        .disabled(!isEnabled)
    }
}
