import SwiftUI

/// Lays out tags left to right, wrapping to new rows (Settings previews).
struct ChipFlowLayout: Layout {
    var spacing: CGFloat = NameChipStyle.spacing
    var lineSpacing: CGFloat = NameChipStyle.lineSpacing

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let rows = arrange(width: proposal.width ?? .infinity, subviews: subviews)
        let width = rows.map(\.width).max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + CGFloat(max(rows.count - 1, 0)) * lineSpacing
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var y = bounds.minY
        for row in arrange(width: bounds.width, subviews: subviews) {
            var x = bounds.minX
            for index in row.indexes {
                let size = subviews[index].sizeThatFits(.unspecified)
                subviews[index].place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
                x += size.width + spacing
            }
            y += row.height + lineSpacing
        }
    }

    private struct Row {
        var indexes: [Int] = []
        var width: CGFloat = 0
        var height: CGFloat = 0
    }

    private func arrange(width: CGFloat, subviews: Subviews) -> [Row] {
        var rows: [Row] = []
        var current = Row()
        for index in subviews.indices {
            let size = subviews[index].sizeThatFits(.unspecified)
            let needed = current.indexes.isEmpty ? size.width : current.width + spacing + size.width
            if needed > width, !current.indexes.isEmpty {
                rows.append(current)
                current = Row()
            }
            current.width = current.indexes.isEmpty ? size.width : current.width + spacing + size.width
            current.height = max(current.height, size.height)
            current.indexes.append(index)
        }
        if !current.indexes.isEmpty { rows.append(current) }
        return rows
    }
}
