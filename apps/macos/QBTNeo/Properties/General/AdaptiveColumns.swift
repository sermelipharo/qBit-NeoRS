/// Column math of `AdaptiveColumnsLayout`, kept free of SwiftUI so it can be
/// unit-tested.
enum AdaptiveColumns {
    /// The widths of the columns for items of the given ideal widths in
    /// `width` (nil: as wide as they like). Uses the most columns that fit,
    /// skipping counts that leave the last row less than half full; each
    /// column takes its widest item and leftover width is shared evenly.
    /// When not even two columns fit, one column takes the whole width.
    static func widths(ideals: [Double], width: Double?, spacing: Double) -> [Double] {
        let count = ideals.count
        guard count > 0 else { return [] }
        for columns in stride(from: count, through: 2, by: -1) {
            let remainder = count % columns
            if remainder != 0, remainder * 2 < columns { continue }
            let natural = (0..<columns).map { column in
                stride(from: column, to: count, by: columns).map { ideals[$0] }.max() ?? 0
            }
            guard let width else { return natural }
            let total = natural.reduce(0, +) + spacing * Double(columns - 1)
            if total <= width {
                let extra = (width - total) / Double(columns)
                return natural.map { $0 + extra }
            }
        }
        return [max(width ?? ideals.max() ?? 0, 0)]
    }
}
