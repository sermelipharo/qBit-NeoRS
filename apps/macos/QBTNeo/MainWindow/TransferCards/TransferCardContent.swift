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
    /// ETA while downloading, the share ratio otherwise.
    var trailingText = ""
}
