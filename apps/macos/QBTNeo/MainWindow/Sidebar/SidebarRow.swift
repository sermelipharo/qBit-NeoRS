import AppKit

/// A sidebar filter row: icon (SF Symbol in its tint, or a tracker
/// favicon), title and count, laid out by hand.
@MainActor
final class SidebarRow: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("sidebar.row")
    /// Fixed icon column so titles line up across rows and sections.
    private static let iconWidth: CGFloat = 18
    private static let gap: CGFloat = 6
    private static let trailing: CGFloat = 6

    private let icon = NSImageView()
    private let title = NSTextField(labelWithString: "")
    private let count = NSTextField(labelWithString: "")
    private var tint: NSColor = .secondaryLabelColor
    private var usesFavicon = false

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        icon.imageScaling = .scaleProportionallyDown
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: NSFont.systemFontSize, weight: .medium)
        icon.setAccessibilityElement(false)
        title.lineBreakMode = .byTruncatingMiddle
        title.cell?.truncatesLastVisibleLine = true
        count.font = NSFont.monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        count.alignment = .right
        for view in [icon, title, count] as [NSView] { addSubview(view) }
        imageView = icon
        textField = title
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in outline: NSOutlineView) -> SidebarRow {
        outline.makeView(withIdentifier: identifier, owner: nil) as? SidebarRow ?? SidebarRow()
    }

    func configure(_ row: SidebarSnapshot.Row, favicon: NSImage?) {
        if title.stringValue != row.title {
            title.stringValue = row.title
            title.toolTip = row.title
        }
        // Like a SwiftUI badge, a zero count shows nothing.
        let torrents = row.count
        let text = torrents > 0 ? torrents.formatted() : ""
        if count.stringValue != text {
            count.stringValue = text
            needsLayout = true
        }
        if usesFavicon != (favicon != nil) {
            usesFavicon = favicon != nil
            needsLayout = true
        }
        let image = favicon ?? SymbolImageCache.image(row.symbol)
        if icon.image !== image { icon.image = image }
        tint = row.tint.color
        setAccessibilityLabel("\(row.title), \(row.count.formatted())")
        applyColors()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { if backgroundStyle != oldValue { applyColors() } }
    }

    private func applyColors() {
        let emphasized = backgroundStyle == .emphasized
        // The selected row is filled with the accent color, so an accent-tinted symbol would vanish into
        // it: draw it in the selection foreground instead.
        icon.contentTintColor = usesFavicon ? nil : (emphasized ? .alternateSelectedControlTextColor : tint)
        title.textColor = emphasized ? .alternateSelectedControlTextColor : .labelColor
        count.textColor = emphasized ? .alternateSelectedControlTextColor : .secondaryLabelColor
    }

    override func layout() {
        super.layout()
        let iconSide = usesFavicon ? CGFloat(TrackerFaviconStore.iconPoints) : Self.iconWidth
        icon.frame = NSRect(x: (Self.iconWidth - iconSide) / 2, y: ((bounds.height - iconSide) / 2).rounded(), width: iconSide, height: iconSide)
        let countWidth = ceil(count.intrinsicContentSize.width) + 2
        let textHeight = ceil(title.intrinsicContentSize.height)
        let y = ((bounds.height - textHeight) / 2).rounded()
        count.frame = NSRect(x: bounds.width - countWidth - Self.trailing, y: y, width: countWidth, height: textHeight)
        let x = Self.iconWidth + Self.gap
        title.frame = NSRect(x: x, y: y, width: max(count.frame.minX - x - Self.gap, 0), height: textHeight)
    }
}
