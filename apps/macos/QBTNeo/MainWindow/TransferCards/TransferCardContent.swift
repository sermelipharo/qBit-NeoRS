import AppKit

/// What one card shows. Cards compare it with what they show and skip
/// identical updates, so refreshing an unchanged card costs nothing.
struct TransferCardContent: Equatable {
    var title = ""
    /// The full torrent name (tooltip and VoiceOver).
    var name = ""
    var kind = ContentKind.other
    var chips: [NameChip] = []
    /// Category, then tags, with their icons and colors (shown as tags after the name's).
    var labels: [TableCellContent.Label] = []
    /// Whether `labels` starts with the category.
    var hasCategory = false
    /// Drawn large in the top-right corner, always in the state color.
    var stateSymbol = ""
    var stateColor: NSColor = .secondaryLabelColor
    var statusText = ""
    /// Nil uses the label color (the "Color by state" setting decides).
    var statusTextColor: NSColor?
    var progress = 0.0
    /// Nil uses the accent color.
    var progressTint: NSColor?
    var dimsProgress = false
    var percentText = ""
    var sizeText = ""
    var downloadText = ""
    var uploadText = ""
    /// Before the size on the status line.
    var ratioText = ""
    /// While downloading, after the speeds.
    var etaText = ""

    /// Speeds or an ETA to show: the card has a second status line.
    var hasActivity: Bool { !downloadText.isEmpty || !uploadText.isEmpty || !etaText.isEmpty }
}
