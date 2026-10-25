import AppKit

/// Name cell of content trees: indentation, disclosure chevron, checkbox
/// (download / do not download, mixed for partly ignored folders), Finder
/// icon and name. Laid out by hand, no constraints.
@MainActor
final class FileNameCell: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("file.name")
    private static let indentPerLevel: CGFloat = 16
    private static let buttonWidth: CGFloat = 14
    private static let iconSize: CGFloat = 16
    private static let gap: CGFloat = 4

    private let chevron = NSButton()
    private let checkbox = NSButton()
    private let icon = NSImageView()
    private let label = NSTextField(labelWithString: "")
    private var depth = 0
    private weak var node: FileNode?
    private var isIgnored = false
    private var emphasized: NSRange?
    private var showsAttributed = false
    var onToggleExpanded: ((FileNode) -> Void)?
    var onToggleChecked: ((FileNode) -> Void)?

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        for button in [chevron, checkbox] {
            button.isBordered = false
            button.bezelStyle = .inline
            button.imagePosition = .imageOnly
            button.imageScaling = .scaleProportionallyDown
            button.target = self
            addSubview(button)
        }
        chevron.action = #selector(toggleExpanded)
        chevron.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: NSFont.smallSystemFontSize, weight: .bold)
        chevron.contentTintColor = .secondaryLabelColor
        checkbox.action = #selector(toggleChecked)
        icon.imageScaling = .scaleProportionallyUpOrDown
        icon.setAccessibilityElement(false)
        label.font = TableTextCell.font
        label.lineBreakMode = .byTruncatingMiddle
        label.cell?.truncatesLastVisibleLine = true
        label.maximumNumberOfLines = 1
        addSubview(icon)
        addSubview(label)
        textField = label
        imageView = icon
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView) -> FileNameCell {
        table.makeView(withIdentifier: identifier, owner: nil) as? FileNameCell ?? FileNameCell()
    }

    func configure(_ node: FileNode, filtering: Bool, emphasized: NSRange? = nil) {
        self.node = node
        if node.depth != depth {
            depth = node.depth
            needsLayout = true
        }
        chevron.isHidden = !node.isFolder
        if node.isFolder {
            let expanded = node.isExpanded || filtering
            chevron.image = SymbolImageCache.image(expanded ? Symbol.chevronDown : Symbol.chevronRight)
            chevron.setAccessibilityLabel(node.isExpanded ? String(localized: "Collapse") : String(localized: "Expand"))
        }
        let state = node.checkState
        let symbol = switch state {
        case .checked: Symbol.checkmarkSquareFill
        case .unchecked: Symbol.square
        case .partial: Symbol.minusSquareFill
        }
        checkbox.image = SymbolImageCache.image(symbol)
        checkbox.setAccessibilityLabel(state == .unchecked ? String(localized: "Download") : String(localized: "Do not download"))
        isIgnored = node.priority == .ignored
        let image = node.isFolder ? FileIconCache.folder : FileIconCache.icon(forExtension: (node.name as NSString).pathExtension)
        if icon.image !== image { icon.image = image }
        self.emphasized = emphasized.flatMap { $0.length > 0 && NSMaxRange($0) <= (node.name as NSString).length ? $0 : nil }
        if self.emphasized == nil, showsAttributed || label.stringValue != node.name {
            label.stringValue = node.name
            showsAttributed = false
        }
        if label.toolTip != node.path { label.toolTip = node.path }
        applyColors()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { if backgroundStyle != oldValue { applyColors() } }
    }

    private func applyColors() {
        let emphasized = backgroundStyle == .emphasized
        let textColor: NSColor = emphasized ? .alternateSelectedControlTextColor : (isIgnored ? .secondaryLabelColor : .labelColor)
        label.textColor = textColor
        if let range = self.emphasized, let name = node?.name {
            // The title semibold, the technical part regular, like the transfer list.
            let text = NSMutableAttributedString(string: name, attributes: [.font: TableTextCell.font, .foregroundColor: textColor])
            text.addAttribute(.font, value: NSFont.systemFont(ofSize: TableTextCell.font.pointSize, weight: .semibold), range: range)
            let paragraph = NSMutableParagraphStyle()
            paragraph.lineBreakMode = .byTruncatingMiddle
            text.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: text.length))
            label.attributedStringValue = text
            showsAttributed = true
        }
        chevron.contentTintColor = emphasized ? .alternateSelectedControlTextColor : .secondaryLabelColor
        let unchecked = node?.checkState == .unchecked
        checkbox.contentTintColor = emphasized ? .alternateSelectedControlTextColor : (unchecked ? .secondaryLabelColor : .controlAccentColor)
    }

    @objc private func toggleExpanded() {
        if let node { onToggleExpanded?(node) }
    }

    @objc private func toggleChecked() {
        if let node { onToggleChecked?(node) }
    }

    override func layout() {
        super.layout()
        var x = CGFloat(depth) * Self.indentPerLevel
        chevron.frame = NSRect(x: x, y: 0, width: Self.buttonWidth, height: bounds.height)
        x += Self.buttonWidth + Self.gap
        checkbox.frame = NSRect(x: x, y: 0, width: Self.buttonWidth + 2, height: bounds.height)
        x += Self.buttonWidth + 2 + Self.gap
        icon.frame = NSRect(x: x, y: ((bounds.height - Self.iconSize) / 2).rounded(), width: Self.iconSize, height: Self.iconSize)
        x += Self.iconSize + Self.gap
        let height = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: x, y: ((bounds.height - height) / 2).rounded(), width: max(bounds.width - x, 0), height: height)
    }
}
