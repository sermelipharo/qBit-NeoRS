import AppKit

/// A lightweight table cell: one label, optionally an icon in front of it,
/// laid out by hand (no constraints). Text and icon turn white on an
/// emphasized (selected, key) row, like Finder's list view.
@MainActor
final class TableTextCell: NSTableCellView {
    static let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
    static let digitsFont = NSFont.monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
    static let codeFont = NSFont.monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
    static let emphasizedFont = NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: .semibold)
    private static let iconSize: CGFloat = 16
    private static let iconGap: CGFloat = 4
    private static let padding: CGFloat = 2

    private let label = NSTextField(labelWithString: "")
    private var iconView: NSImageView?
    private var content: TableCellContent?
    private var showsAttributed = false

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        label.font = Self.font
        label.lineBreakMode = .byTruncatingTail
        label.cell?.truncatesLastVisibleLine = true
        label.maximumNumberOfLines = 1
        label.allowsDefaultTighteningForTruncation = false
        addSubview(label)
        textField = label
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    /// Returns a reused cell of `table` or a new one.
    static func make(in table: NSTableView, identifier: NSUserInterfaceItemIdentifier) -> TableTextCell {
        table.makeView(withIdentifier: identifier, owner: nil) as? TableTextCell ?? TableTextCell(identifier: identifier)
    }

    func configure(_ new: TableCellContent) {
        guard new != content else { return }
        let old = content
        content = new
        if new.monospacedDigits != old?.monospacedDigits || new.monospaced != old?.monospaced {
            label.font = new.monospaced ? Self.codeFont : (new.monospacedDigits ? Self.digitsFont : Self.font)
        }
        if label.isSelectable != new.isSelectable { label.isSelectable = new.isSelectable }
        label.alignment = new.alignment
        label.lineBreakMode = new.maximumLines > 1 ? .byWordWrapping : new.lineBreakMode
        if label.maximumNumberOfLines != new.maximumLines {
            label.maximumNumberOfLines = new.maximumLines
            label.cell?.wraps = new.maximumLines > 1
            needsLayout = true
        }
        if label.toolTip != new.toolTip { label.toolTip = new.toolTip }
        configureIcon(new.icon)
        applyText()
        if old?.icon == nil && new.icon != nil || old?.icon != nil && new.icon == nil || old?.indent != new.indent {
            needsLayout = true
        }
    }

    private func configureIcon(_ icon: TableCellContent.Icon?) {
        guard let icon else {
            iconView?.isHidden = true
            imageView = nil
            return
        }
        let view: NSImageView
        if let iconView {
            view = iconView
        } else {
            view = NSImageView()
            view.imageScaling = .scaleProportionallyDown
            addSubview(view)
            iconView = view
            needsLayout = true
        }
        view.isHidden = false
        imageView = view
        let image = icon.image ?? icon.symbol.flatMap { SymbolImageCache.image($0) }
        if view.image !== image { view.image = image }
        view.setAccessibilityElement(icon.accessibilityDescription != nil)
        view.setAccessibilityLabel(icon.accessibilityDescription)
        applyIconColor()
    }

    // MARK: Selection

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet {
            guard backgroundStyle != oldValue else { return }
            applyText()
            applyIconColor()
        }
    }

    private var isEmphasized: Bool { backgroundStyle == .emphasized }

    private func applyText() {
        guard let content else { return }
        let color = isEmphasized ? NSColor.alternateSelectedControlTextColor : (content.color ?? .labelColor)
        if let range = content.emphasizedRange {
            showsAttributed = true
            label.attributedStringValue = Self.emphasizedString(content, range: range, color: color)
            label.setAccessibilityLabel(content.accessibilityLabel)
        } else if !content.prefix.isEmpty {
            showsAttributed = true
            label.attributedStringValue = Self.prefixedString(content, emphasized: isEmphasized)
            label.setAccessibilityLabel(content.accessibilityLabel)
        } else if content.labels.isEmpty {
            label.textColor = color
            if showsAttributed || label.stringValue != content.text {
                label.stringValue = content.text
            }
            label.setAccessibilityLabel(content.accessibilityLabel)
            showsAttributed = false
        } else {
            showsAttributed = true
            label.attributedStringValue = Self.labelsString(content.labels, textColor: color, emphasized: isEmphasized,
                                                            alignment: content.alignment)
            label.setAccessibilityLabel(content.labels.map(\.name).joined(separator: ", "))
        }
    }

    /// `prefix` in its color followed by `text`, wrapped like the label.
    static func prefixedString(_ content: TableCellContent, emphasized: Bool) -> NSAttributedString {
        let font = content.monospacedDigits ? digitsFont : font
        let textColor = emphasized ? NSColor.alternateSelectedControlTextColor : (content.color ?? .labelColor)
        let prefixColor = emphasized ? textColor : (content.prefixColor ?? textColor)
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byWordWrapping
        paragraph.alignment = content.alignment
        let result = NSMutableAttributedString(string: content.prefix, attributes: [
            .font: font, .foregroundColor: prefixColor, .paragraphStyle: paragraph,
        ])
        result.append(NSAttributedString(string: content.text, attributes: [
            .font: font, .foregroundColor: textColor, .paragraphStyle: paragraph,
        ]))
        return result
    }

    /// `text` in the regular font with `range` semibold, truncated like the label.
    /// Built only when the content or the selection changes (cells skip identical content).
    static func emphasizedString(_ content: TableCellContent, range: NSRange, color: NSColor) -> NSAttributedString {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = content.lineBreakMode
        paragraph.alignment = content.alignment
        let result = NSMutableAttributedString(string: content.text, attributes: [
            .font: font, .foregroundColor: color, .paragraphStyle: paragraph,
        ])
        let clamped = NSIntersectionRange(range, NSRange(location: 0, length: result.length))
        if clamped.length > 0 { result.addAttribute(.font, value: emphasizedFont, range: clamped) }
        return result
    }

    /// Row height that fits `content` wrapped to `width` (at most `maximumLines` lines).
    static func height(of content: TableCellContent, width: CGFloat, minimum: CGFloat) -> CGFloat {
        let string = content.prefix.isEmpty
            ? NSAttributedString(string: content.text, attributes: [.font: content.monospacedDigits ? digitsFont : font])
            : prefixedString(content, emphasized: false)
        let lineHeight = ceil(font.ascender - font.descender + font.leading)
        let bounds = string.boundingRect(with: NSSize(width: max(width - 2 * padding, 1), height: .greatestFiniteMagnitude),
                                         options: [.usesLineFragmentOrigin, .usesFontLeading])
        let lines = min(max(Int(ceil(bounds.height / lineHeight)), 1), max(content.maximumLines, 1))
        return max(minimum, CGFloat(lines) * lineHeight + 8)
    }

    private func applyIconColor() {
        guard let iconView, let icon = content?.icon else { return }
        iconView.contentTintColor = isEmphasized ? .alternateSelectedControlTextColor : icon.color
    }

    /// Names with their icon in their color, as in the category and tag editors.
    private static func labelsString(_ labels: [TableCellContent.Label], textColor: NSColor, emphasized: Bool,
                                     alignment: NSTextAlignment) -> NSAttributedString {
        let result = NSMutableAttributedString()
        let attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: textColor]
        let configuration = NSImage.SymbolConfiguration(pointSize: NSFont.smallSystemFontSize, weight: .regular)
        for (index, item) in labels.enumerated() {
            if index > 0 { result.append(NSAttributedString(string: "   ", attributes: attributes)) }
            let color = emphasized ? NSColor.alternateSelectedControlTextColor : item.color
            if let image = SymbolImageCache.image(item.symbol)?
                .withSymbolConfiguration(configuration.applying(NSImage.SymbolConfiguration(paletteColors: [color])).applying(.preferringMonochrome())) {
                let attachment = NSTextAttachment()
                attachment.image = image
                let height = image.size.height
                attachment.bounds = CGRect(x: 0, y: (font.capHeight - height) / 2, width: image.size.width, height: height)
                result.append(NSAttributedString(attachment: attachment))
                result.append(NSAttributedString(string: "\u{2009}", attributes: attributes))
            }
            result.append(NSAttributedString(string: item.name, attributes: attributes))
        }
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byTruncatingTail
        paragraph.alignment = alignment
        result.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: result.length))
        return result
    }

    // MARK: Layout

    override func layout() {
        super.layout()
        let indent = content?.indent ?? 0
        var x = Self.padding + indent
        if let iconView, !iconView.isHidden {
            let size = Self.iconSize
            iconView.frame = NSRect(x: x, y: ((bounds.height - size) / 2).rounded(), width: size, height: size)
            x += size + Self.iconGap
        }
        if label.maximumNumberOfLines > 1 {
            label.frame = NSRect(x: x, y: 3, width: max(bounds.width - x - Self.padding, 0), height: max(bounds.height - 6, 0))
            return
        }
        let height = ceil(label.intrinsicContentSize.height)
        label.frame = NSRect(x: x, y: ((bounds.height - height) / 2).rounded(), width: max(bounds.width - x - Self.padding, 0),
                             height: height)
    }

    /// The width that shows the whole label (column autosizing).
    var fittingWidth: CGFloat {
        var width = Self.padding * 2 + (content?.indent ?? 0) + ceil(label.intrinsicContentSize.width)
        if let iconView, !iconView.isHidden { width += Self.iconSize + Self.iconGap }
        return width
    }
}
