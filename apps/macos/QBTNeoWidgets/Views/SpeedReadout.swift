import SwiftUI
import WidgetKit

/// A direction arrow with the current speed; the number is set larger than its unit.
struct SpeedReadout: View {
    enum Direction {
        case download, upload

        var symbol: String { self == .download ? "arrow.down" : "arrow.up" }
        var color: Color { self == .download ? WidgetPalette.download : WidgetPalette.upload }
        var label: String { self == .download ? String(localized: "Download speed") : String(localized: "Upload speed") }
    }

    let direction: Direction
    let rate: Int64
    var numberSize = 22.0

    var body: some View {
        let parts = WidgetFormat.speedParts(rate)
        HStack(alignment: .firstTextBaseline, spacing: 4) {
            Image(systemName: direction.symbol)
                .font(.system(size: numberSize * 0.62, weight: .bold))
                .foregroundStyle(direction.color)
                .widgetAccentable()
            Text(parts.number)
                .font(.system(size: numberSize, weight: .semibold, design: .rounded))
                .monospacedDigit()
                .contentTransition(.numericText(value: Double(rate)))
            Text(parts.unit)
                .font(.system(size: max(numberSize * 0.5, 11), weight: .medium))
                .foregroundStyle(.secondary)
        }
        .lineLimit(1)
        .minimumScaleFactor(0.6)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(direction.label))
        .accessibilityValue(Text(WidgetFormat.speed(rate)))
    }
}
