import SwiftUI

/// One glanceable value of the General tab's summary strip: a tinted symbol,
/// the value and a caption below it.
struct GeneralMetric: View {
    let title: LocalizedStringKey
    let symbol: String
    let tint: Color
    let value: String
    let caption: String
    var valueStyle: Color?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: Design.Spacing.small) {
            Image(systemName: symbol)
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(tint)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 0) {
                Text(value)
                    .monospacedDigit()
                    .foregroundStyle(valueStyle ?? .primary)
                    .modifier(SelectableValue())
                Text(caption)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
            }
            .lineLimit(1)
        }
        .font(.title3)
        .frame(maxWidth: .infinity, alignment: .leading)
        .help(Text(title))
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text(title))
        .accessibilityValue(Text(verbatim: "\(value), \(caption)"))
    }
}
