import AppKit

/// Sizes of the cards view (points). Cards are as wide as the columns that
/// fit (240–380 pt). A card is as tall as its content: the title takes one
/// or two lines, the tags none, one or two rows, and the status one or two
/// lines (the second only with speeds or an ETA); the cards of one grid row
/// share the tallest one's height.
///
/// Inside a card, pinned to the top: 12 pt inset, kind symbol, title and
/// state symbol, 8 pt, tags. Pinned to the bottom: progress bar, 8 pt, the
/// status lines, 12 pt inset. At least 8 pt between the two blocks.
@MainActor enum TransferCardMetrics {
    static let minWidth: CGFloat = 240
    static let maxWidth: CGFloat = 380
    /// Between cards and around the grid.
    static let spacing: CGFloat = 12
    /// Inner insets of a card.
    static let padding: CGFloat = 12
    static let cornerRadius: CGFloat = 10
    static let titleLines = 2
    static let chipLines = 2
    /// Title 3 of the macOS type scale (15 pt), semibold.
    static let titleFont = NSFont.systemFont(ofSize: NSFont.preferredFont(forTextStyle: .title3).pointSize, weight: .semibold)
    static let infoFont = NSFont.monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
    /// The status word: the info size in bold SF Pro Rounded.
    static let statusFont: NSFont = {
        let bold = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize, weight: .bold)
        return bold.fontDescriptor.withDesign(.rounded).flatMap { NSFont(descriptor: $0, size: 0) } ?? bold
    }()
    /// Between the title, the tags, the progress bar and the status lines.
    static let groupSpacing: CGFloat = 8
    static let progressHeight: CGFloat = 4
    static let infoLineSpacing: CGFloat = 2
    /// The state symbol in the top-right corner (a square box, centered on the
    /// title's first line), and the gap between it and the title.
    static let stateSymbolBox: CGFloat = 18
    static let stateSymbolGap: CGFloat = 8
    /// The content kind symbol before the title, and the title's indent after it.
    static let kindSymbolSize: CGFloat = 13
    static let titleIndent: CGFloat = 22

    static func lineHeight(_ font: NSFont) -> CGFloat {
        ceil(font.ascender - font.descender + font.leading)
    }

    static var titleLineHeight: CGFloat { lineHeight(titleFont) }
    static var infoLineHeight: CGFloat { lineHeight(infoFont) }

    /// Height of `lines` status lines.
    static func infoHeight(lines: Int) -> CGFloat {
        CGFloat(lines) * infoLineHeight + CGFloat(max(lines - 1, 0)) * infoLineSpacing
    }

    /// The progress bar and the status lines, from the bar's top to the card's bottom edge.
    static func bottomBlockHeight(infoLines: Int) -> CGFloat {
        progressHeight + groupSpacing + infoHeight(lines: infoLines) + padding
    }

    /// Width of the title's text area in a card of `cardWidth` (between the kind and state symbols).
    static func titleWidth(cardWidth: CGFloat) -> CGFloat {
        max(cardWidth - 2 * padding - titleIndent - stateSymbolBox - stateSymbolGap, 0)
    }

    /// Title lines for a title whose single-line width is `naturalWidth`.
    static func titleLines(naturalWidth: CGFloat, cardWidth: CGFloat) -> Int {
        naturalWidth <= titleWidth(cardWidth: cardWidth) ? 1 : titleLines
    }

    /// Single-line width of a title in the title font.
    static func naturalTitleWidth(_ title: String) -> CGFloat {
        ceil((title as NSString).size(withAttributes: [.font: titleFont]).width)
    }

    static func height(titleLines: Int, chipLines: Int, infoLines: Int) -> CGFloat {
        var height = padding + CGFloat(titleLines) * titleLineHeight
        if chipLines > 0 { height += groupSpacing + NameChipLayout.height(lines: chipLines) }
        height += groupSpacing + bottomBlockHeight(infoLines: infoLines)
        return ceil(height)
    }
}
