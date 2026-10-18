import AppKit

/// What a `TableTextCell` shows. Cells compare it with what they show and
/// skip identical updates, so refreshing an unchanged cell costs nothing.
struct TableCellContent: Equatable {
    /// A symbol before a name in a labels cell (category / tag).
    struct Label: Equatable {
        var name: String
        var symbol: String
        var color: NSColor
    }

    /// The leading icon (state symbol of a torrent, file icon).
    struct Icon: Equatable {
        var symbol: String?
        var image: NSImage?
        var color: NSColor?
        /// Spoken by VoiceOver instead of the image.
        var accessibilityDescription: String?
    }

    var text = ""
    /// Nil uses the label color; selection-emphasized rows always use white.
    var color: NSColor?
    var icon: Icon?
    /// When non-empty the cell shows these instead of `text`.
    var labels: [Label] = []
    var monospacedDigits = false
    /// Fixed-width font for codes (peer flags, IDs).
    var monospaced = false
    /// The text can be selected and copied (IP addresses).
    var isSelectable = false
    var alignment: NSTextAlignment = .natural
    var lineBreakMode: NSLineBreakMode = .byTruncatingTail
    /// Only long-text columns set one (each tooltip region costs memory).
    var toolTip: String?
    var indent: CGFloat = 0
    /// Text in front of `text` in its own color (log timestamps).
    var prefix = ""
    var prefixColor: NSColor?
    /// Wrapped lines shown before truncating (log messages); rows must be tall enough.
    var maximumLines = 1
    /// Spoken by VoiceOver instead of the text.
    var accessibilityLabel: String?
    /// Part of `text` drawn semibold (a torrent's title within its name).
    var emphasizedRange: NSRange?

    init(
        _ text: String = "", color: NSColor? = nil, icon: Icon? = nil, labels: [Label] = [], monospacedDigits: Bool = false,
        monospaced: Bool = false, isSelectable: Bool = false, alignment: NSTextAlignment = .natural,
        lineBreakMode: NSLineBreakMode = .byTruncatingTail, toolTip: String? = nil, indent: CGFloat = 0
    ) {
        self.text = text
        self.color = color
        self.icon = icon
        self.labels = labels
        self.monospacedDigits = monospacedDigits
        self.monospaced = monospaced
        self.isSelectable = isSelectable
        self.alignment = alignment
        self.lineBreakMode = lineBreakMode
        self.toolTip = toolTip
        self.indent = indent
    }
}
