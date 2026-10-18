import AppKit

/// Sizes of the cards view (points). Cards are as wide as the columns that
/// fit (240–380 pt). A card is as tall as its content: the title takes one
/// or two lines and the tags none, one or two rows; the cards of one grid
/// row share the tallest one's height.
///
/// Inside a card, top to bottom: 12 pt inset, kind symbol and title, 8 pt,
/// tags, 8 pt, progress bar, 8 pt, two lines of status, 12 pt inset.
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
    /// Between the title, the tags, the progress bar and the status lines.
    static let groupSpacing: CGFloat = 8
    static let progressHeight: CGFloat = 4
    static let infoLines = 2
    static let infoLineSpacing: CGFloat = 2
    static let iconSize: CGFloat = 12
    static let iconGap: CGFloat = 4
    /// The content kind symbol before the title, and the title's indent after it.
    static let kindSymbolSize: CGFloat = 13
    static let titleIndent: CGFloat = 22

    static func lineHeight(_ font: NSFont) -> CGFloat {
        ceil(font.ascender - font.descender + font.leading)
    }

    static var titleLineHeight: CGFloat { lineHeight(titleFont) }
    static var infoHeight: CGFloat {
        CGFloat(infoLines) * lineHeight(infoFont) + CGFloat(infoLines - 1) * infoLineSpacing
    }

    /// Width of the title's text area in a card of `cardWidth`.
    static func titleWidth(cardWidth: CGFloat) -> CGFloat {
        max(cardWidth - 2 * padding - titleIndent, 0)
    }

    /// Title lines for a title whose single-line width is `naturalWidth`.
    static func titleLines(naturalWidth: CGFloat, cardWidth: CGFloat) -> Int {
        naturalWidth <= titleWidth(cardWidth: cardWidth) ? 1 : titleLines
    }

    /// Single-line width of a title in the title font.
    static func naturalTitleWidth(_ title: String) -> CGFloat {
        ceil((title as NSString).size(withAttributes: [.font: titleFont]).width)
    }

    static func height(titleLines: Int, chipLines: Int) -> CGFloat {
        var height = padding + CGFloat(titleLines) * titleLineHeight
        if chipLines > 0 { height += groupSpacing + NameChipLayout.height(lines: chipLines) }
        height += groupSpacing + progressHeight + groupSpacing + infoHeight + padding
        return ceil(height)
    }
}
