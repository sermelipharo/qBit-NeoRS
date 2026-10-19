import SwiftUI

/// The name's tags on one line: as many as fit, in order; the rest are left
/// out (a tag is never cut).
struct NameChipRow: View {
    let chips: [WidgetNameChip]

    var body: some View {
        FirstFitRow(spacing: 4) {
            ForEach(Array(chips.enumerated()), id: \.offset) { _, chip in
                NameChipView(chip: chip)
            }
        }
        .clipped()
    }
}

/// Lays its subviews out left to right at their ideal sizes and stops at the
/// first one that does not fit; the others are placed outside the bounds.
private struct FirstFitRow: Layout {
    var spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout Void) -> CGSize {
        let sizes = subviews.map { $0.sizeThatFits(.unspecified) }
        let height = sizes.map(\.height).max() ?? 0
        guard let limit = proposal.width else {
            let width = sizes.map(\.width).reduce(0, +) + spacing * CGFloat(max(sizes.count - 1, 0))
            return CGSize(width: width, height: height)
        }
        return CGSize(width: limit, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout Void) {
        var x = bounds.minX
        var isFull = false
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            isFull = isFull || x + size.width > bounds.maxX + 0.5
            let origin = isFull ? CGPoint(x: bounds.maxX + 10_000, y: bounds.minY) : CGPoint(x: x, y: bounds.minY)
            subview.place(at: origin, proposal: ProposedViewSize(size))
            x += size.width + spacing
        }
    }
}
