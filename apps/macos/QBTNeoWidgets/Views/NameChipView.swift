import SwiftUI
import WidgetKit

/// One technical tag of the name, drawn like the cards' tags: SF Rounded
/// 11 pt medium on the tint at 16 % (dark 26 %), or outlined; the text is the
/// tint darkened (light) or lightened (dark) so yellow and mint stay readable.
struct NameChipView: View {
    let chip: WidgetNameChip
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.widgetRenderingMode) private var renderingMode

    var body: some View {
        let tint = chip.tint.color
        let isDark = colorScheme == .dark
        let isFullColor = renderingMode == .fullColor
        Text(chip.text)
            .font(.system(size: 11, weight: .medium, design: .rounded))
            .foregroundStyle(isFullColor ? tint.mix(with: isDark ? .white : .black, by: isDark ? 0.35 : 0.5) : .primary)
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 6)
            .frame(height: 18)
            .background {
                let shape = RoundedRectangle(cornerRadius: 5, style: .continuous)
                if chip.isOutlined {
                    shape.strokeBorder(tint.opacity(isDark ? 0.6 : 0.55), lineWidth: 1)
                } else {
                    shape.fill(tint.opacity(isDark ? 0.26 : 0.16))
                }
            }
    }
}
