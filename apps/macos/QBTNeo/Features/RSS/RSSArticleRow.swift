import AppKit

/// An article row: a dot (filled while unread), the title in the link color
/// while unread (at most two lines) and the feed and date below it.
@MainActor
final class RSSArticleRow: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("rss.article")
    static let titleFont = NSFont.systemFont(ofSize: NSFont.systemFontSize)
    static let subtitleFont = NSFont.systemFont(ofSize: NSFont.smallSystemFontSize - 1)
    private static let dotWidth: CGFloat = 12
    private static let gap: CGFloat = 8
    private static let vertical: CGFloat = 4

    private let dot = NSImageView()
    private let title = NSTextField(wrappingLabelWithString: "")
    private let subtitle = NSTextField(labelWithString: "")
    private var isRead = false

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        dot.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: NSFont.smallSystemFontSize - 2, weight: .regular)
        title.font = Self.titleFont
        title.maximumNumberOfLines = 2
        title.cell?.truncatesLastVisibleLine = true
        title.lineBreakMode = .byWordWrapping
        subtitle.font = Self.subtitleFont
        subtitle.lineBreakMode = .byTruncatingTail
        for view in [dot, title, subtitle] as [NSView] { addSubview(view) }
        textField = title
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView) -> RSSArticleRow {
        table.makeView(withIdentifier: identifier, owner: nil) as? RSSArticleRow ?? RSSArticleRow()
    }

    func configure(_ item: RSSArticleItem, showsFeed: Bool) {
        isRead = item.article.isRead
        dot.image = SymbolImageCache.image(isRead ? Symbol.circle : Symbol.circleFill)
        dot.setAccessibilityLabel(isRead ? String(localized: "Read") : String(localized: "Unread"))
        if title.stringValue != item.article.title { title.stringValue = item.article.title }
        let text = Self.subtitle(item, showsFeed: showsFeed)
        if subtitle.stringValue != text { subtitle.stringValue = text }
        applyColors()
    }

    static func subtitle(_ item: RSSArticleItem, showsFeed: Bool) -> String {
        let date = item.article.date > 0 ? ValueFormatter.date(item.article.date) : ""
        return showsFeed ? [item.feedTitle, date].filter { !$0.isEmpty }.joined(separator: " · ") : date
    }

    /// Height of a row whose title wraps in `width`.
    static func height(_ item: RSSArticleItem, width: CGFloat) -> CGFloat {
        let titleWidth = max(width - dotWidth - gap - 4, 1)
        let bounds = (item.article.title as NSString).boundingRect(
            with: NSSize(width: titleWidth, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading], attributes: [.font: titleFont]
        )
        let line = ceil(titleFont.ascender - titleFont.descender + titleFont.leading)
        let lines = min(max(Int(ceil(bounds.height / line)), 1), 2)
        let subtitleLine = ceil(subtitleFont.ascender - subtitleFont.descender + subtitleFont.leading)
        return CGFloat(lines) * line + 2 + subtitleLine + 2 * vertical
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { if backgroundStyle != oldValue { applyColors() } }
    }

    private func applyColors() {
        let emphasized = backgroundStyle == .emphasized
        let white = NSColor.alternateSelectedControlTextColor
        dot.contentTintColor = emphasized ? white : (isRead ? .tertiaryLabelColor : .controlAccentColor)
        title.textColor = emphasized ? white : (isRead ? .secondaryLabelColor : .linkColor)
        subtitle.textColor = emphasized ? white.withAlphaComponent(0.8) : .tertiaryLabelColor
    }

    override var isFlipped: Bool { true }

    override func layout() {
        super.layout()
        let x = Self.dotWidth + Self.gap
        let width = max(bounds.width - x - 4, 0)
        let subtitleHeight = ceil(subtitle.intrinsicContentSize.height)
        let titleHeight = max(bounds.height - 2 * Self.vertical - subtitleHeight - 2, 0)
        title.frame = NSRect(x: x, y: Self.vertical, width: width, height: titleHeight)
        subtitle.frame = NSRect(x: x, y: Self.vertical + titleHeight + 2, width: width, height: subtitleHeight)
        let line = ceil(Self.titleFont.ascender - Self.titleFont.descender)
        dot.frame = NSRect(x: 2, y: Self.vertical, width: Self.dotWidth, height: line)
    }
}
