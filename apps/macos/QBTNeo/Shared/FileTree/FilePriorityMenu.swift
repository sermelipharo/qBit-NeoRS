import AppKit

/// Priority cell of content trees: a borderless pull-down showing the
/// current priority ("Mixed" for folders whose children differ).
@MainActor
final class FilePriorityMenu: NSTableCellView {
    static let identifier = NSUserInterfaceItemIdentifier("file.priority")

    private let button = NSPopUpButton(frame: .zero, pullsDown: true)
    private weak var node: FileNode?
    private var isIgnored = false
    var onSelect: ((FileNode, FilePriorityChoice) -> Void)?

    init() {
        super.init(frame: .zero)
        identifier = Self.identifier
        button.isBordered = false
        button.font = TableTextCell.font
        (button.cell as? NSPopUpButtonCell)?.arrowPosition = .arrowAtBottom
        let menu = button.menu ?? NSMenu()
        menu.addItem(NSMenuItem(title: "", action: nil, keyEquivalent: ""))
        for choice in FilePriorityChoice.selectable {
            let item = NSMenuItem(title: choice.title, action: #selector(choose(_:)), keyEquivalent: "")
            item.target = self
            item.tag = choice.rawValue
            menu.addItem(item)
        }
        button.menu = menu
        addSubview(button)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView) -> FilePriorityMenu {
        table.makeView(withIdentifier: identifier, owner: nil) as? FilePriorityMenu ?? FilePriorityMenu()
    }

    func configure(_ node: FileNode) {
        self.node = node
        let title = node.priority.title
        if button.title != title {
            button.setTitle(title)
            needsLayout = true
        }
        button.setAccessibilityLabel(String(localized: "Download priority: \(title)"))
        isIgnored = node.priority == .ignored
        applyColor()
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { if backgroundStyle != oldValue { applyColor() } }
    }

    private func applyColor() {
        let color: NSColor = backgroundStyle == .emphasized ? .alternateSelectedControlTextColor
            : (isIgnored ? .secondaryLabelColor : .labelColor)
        button.contentTintColor = color
        // A pull-down shows its first item, whose attributed title carries the color.
        let attributes: [NSAttributedString.Key: Any] = [.foregroundColor: color, .font: TableTextCell.font]
        button.item(at: 0)?.attributedTitle = NSAttributedString(string: button.title, attributes: attributes)
    }

    @objc private func choose(_ sender: NSMenuItem) {
        guard let node, let choice = FilePriorityChoice(rawValue: sender.tag) else { return }
        onSelect?(node, choice)
    }

    override func layout() {
        super.layout()
        let size = button.intrinsicContentSize
        button.frame = NSRect(x: 0, y: ((bounds.height - size.height) / 2).rounded(), width: min(size.width, bounds.width),
                              height: size.height)
    }
}
