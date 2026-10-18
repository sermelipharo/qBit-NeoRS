import SwiftUI

/// One tag in SwiftUI (Settings preview and the category picker), drawn
/// like the cards' tags. VoiceOver reads the text and the category.
struct NameChipLabel: View {
    let text: String
    let category: NameRuleCategory

    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiateWithoutColor

    var body: some View {
        let colors = NameChipStyle.colors(category)
        HStack(spacing: NameChipStyle.symbolGap) {
            if differentiateWithoutColor {
                Image(systemName: NameChipStyle.symbol(category))
                    .font(.system(size: NameChipStyle.symbolSize, weight: .medium))
            }
            Text(text)
                .font(Font(NameChipStyle.font))
                .lineLimit(1)
        }
        .foregroundStyle(Color(nsColor: colors.text))
        .padding(.horizontal, NameChipStyle.horizontalPadding)
        .frame(height: NameChipStyle.height)
        .background(Color(nsColor: colors.fill), in: .rect(cornerRadius: NameChipStyle.cornerRadius))
        .overlay {
            if let border = colors.border {
                RoundedRectangle(cornerRadius: NameChipStyle.cornerRadius)
                    .strokeBorder(Color(nsColor: border), lineWidth: 1)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: "\(text), \(category.title)"))
    }
}

extension NameChipLabel {
    init(_ chip: NameChip) {
        self.init(text: chip.text, category: chip.category)
    }
}
