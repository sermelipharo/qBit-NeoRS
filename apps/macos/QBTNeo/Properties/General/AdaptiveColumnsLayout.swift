import SwiftUI

/// Lays its subviews out in as many columns as fit the width, row by row
/// (column math: `AdaptiveColumns`). Ideal widths are capped at
/// `maxIdealWidth`, so one long value cannot force a single column; it
/// truncates instead.
struct AdaptiveColumnsLayout: Layout {
    var horizontalSpacing: Double
    var verticalSpacing: Double
    var maxIdealWidth: Double = .infinity

    struct Arrangement {
        var columnWidths: [Double] = []
        var rowHeights: [Double] = []
    }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? Double($0) : nil }
        let arrangement = arrange(width: width, subviews: subviews)
        let columns = arrangement.columnWidths
        let natural = columns.reduce(0, +) + horizontalSpacing * Double(max(columns.count - 1, 0))
        let rows = arrangement.rowHeights
        let height = rows.reduce(0, +) + verticalSpacing * Double(max(rows.count - 1, 0))
        return CGSize(width: width ?? natural, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let arrangement = arrange(width: Double(bounds.width), subviews: subviews)
        let columns = max(arrangement.columnWidths.count, 1)
        var y = bounds.minY
        for (row, height) in arrangement.rowHeights.enumerated() {
            var x = bounds.minX
            for column in 0..<columns {
                let index = row * columns + column
                guard index < subviews.count else { break }
                let width = arrangement.columnWidths[column]
                subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(width: width, height: height))
                x += width + horizontalSpacing
            }
            y += height + verticalSpacing
        }
    }

    private func arrange(width: Double?, subviews: Subviews) -> Arrangement {
        let ideals = subviews.map { min(Double($0.sizeThatFits(.unspecified).width), maxIdealWidth) }
        let widths = AdaptiveColumns.widths(ideals: ideals, width: width, spacing: horizontalSpacing)
        guard !widths.isEmpty else { return Arrangement() }
        let columns = widths.count
        let rows = (subviews.count + columns - 1) / columns
        let heights = (0..<rows).map { row in
            (0..<columns).compactMap { column -> Double? in
                let index = row * columns + column
                guard index < subviews.count else { return nil }
                return Double(subviews[index].sizeThatFits(ProposedViewSize(width: widths[column], height: nil)).height)
            }.max() ?? 0
        }
        return Arrangement(columnWidths: widths, rowHeights: heights)
    }
}
