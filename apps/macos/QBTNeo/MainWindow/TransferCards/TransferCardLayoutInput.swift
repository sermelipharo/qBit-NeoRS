import AppKit

/// The parts of a card that decide its height (`TransferCardHeights`).
struct TransferCardLayoutInput: Equatable {
    var title: String
    var chips: [NameChip]
    var labels: [TableCellContent.Label] = []
    /// Speeds or an ETA: a second status line.
    var hasActivity = false
}
