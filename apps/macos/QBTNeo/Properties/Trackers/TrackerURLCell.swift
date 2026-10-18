import AppKit

/// URL cell of the Trackers tab: a disclosure chevron for trackers with
/// announce endpoints, indented endpoint rows and grey sticky rows.
@MainActor
final class TrackerURLCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("tracker.url")
    private static let chevronWidth: CGFloat = 14
    private static let endpointIndent: CGFloat = 16

    private let button = NSButton()
    private let text = TableTextCell(identifier: NSUserInterfaceItemIdentifier("tracker.url.text"))
    private var isEndpoint = false
    private var url = ""
    var toggle: ((String) -> Void)?

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        button.isBordered = false
        button.bezelStyle = .inline
        button.imagePosition = .imageOnly
        button.imageScaling = .scaleProportionallyDown
        button.contentTintColor = .secondaryLabelColor
        button.target = self
        button.action = #selector(toggled)
        button.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: NSFont.smallSystemFontSize, weight: .bold)
        addSubview(button)
        addSubview(text)
        textField = text.textField
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView) -> TrackerURLCell {
        table.makeView(withIdentifier: identifier, owner: nil) as? TrackerURLCell ?? TrackerURLCell()
    }

    func configure(_ row: TrackerRow, expanded: Bool) {
        url = row.url
        let endpoint = if case .endpoint = row.kind { true } else { false }
        if endpoint != isEndpoint {
            isEndpoint = endpoint
            needsLayout = true
        }
        button.isHidden = !row.hasEndpoints
        if row.hasEndpoints {
            let symbol = expanded ? Symbol.chevronDown : Symbol.chevronRight
            button.image = SymbolImageCache.image(symbol)
            button.setAccessibilityLabel(expanded ? String(localized: "Collapse") : String(localized: "Expand"))
        }
        text.configure(TableCellContent(row.url, color: row.kind == .sticky ? .secondaryLabelColor : nil,
                                        lineBreakMode: .byTruncatingMiddle, toolTip: row.url))
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            text.backgroundStyle = backgroundStyle
            button.contentTintColor = backgroundStyle == .emphasized ? .alternateSelectedControlTextColor : .secondaryLabelColor
        }
    }

    @objc private func toggled() {
        toggle?(url)
    }

    override func layout() {
        super.layout()
        let x = isEndpoint ? Self.endpointIndent : 0
        button.frame = NSRect(x: x, y: 0, width: Self.chevronWidth, height: bounds.height)
        let textX = x + Self.chevronWidth + 2
        text.frame = NSRect(x: textX, y: 0, width: max(bounds.width - textX, 0), height: bounds.height)
    }
}
