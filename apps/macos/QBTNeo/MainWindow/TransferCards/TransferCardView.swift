import AppKit

/// One card, drawn in a single view (no subviews): the content kind symbol
/// and the title (semibold, one or two lines), the name's tags followed by
/// the category and tags, the progress bar, then status and percent with the
/// size, and speeds with the ETA or ratio. Content is top-aligned; a card in
/// a row with a taller one keeps the extra height at its bottom. Redraws only
/// when its content, selection or width changes. VoiceOver reads it as one
/// element with a summary, and each tag as a child with its category.
@MainActor
final class TransferCardView: NSView {
    var content = TransferCardContent() {
        didSet {
            guard content != oldValue else { return }
            if content.name != oldValue.name { toolTip = content.name }
            if content.title != oldValue.title { titleNaturalWidth = TransferCardMetrics.naturalTitleWidth(content.title) }
            invalidateChipLayout()
            needsDisplay = true
        }
    }

    var isSelected = false {
        didSet { if isSelected != oldValue { needsDisplay = true } }
    }

    /// Selected in the key window's focused cards view (accent outline) rather than gray.
    var isEmphasized = false {
        didSet { if isEmphasized != oldValue, isSelected { needsDisplay = true } }
    }

    /// Targeted by an open context menu (an outline like the table's focus ring).
    var isMenuTarget = false {
        didSet { if isMenuTarget != oldValue { needsDisplay = true } }
    }

    private var chipLayout: NameChipLayout?
    private var chipLayoutWidth: CGFloat = 0
    private var titleNaturalWidth: CGFloat = 0

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layerContentsRedrawPolicy = .onSetNeedsDisplay
        setAccessibilityElement(true)
        setAccessibilityRole(.group)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override var isFlipped: Bool { true }
    override var isOpaque: Bool { false }

    override func setFrameSize(_ newSize: NSSize) {
        if newSize.width != frame.width { invalidateChipLayout() }
        // The layer redraws only on request (`.onSetNeedsDisplay`): a new size needs a new drawing.
        if newSize != frame.size { needsDisplay = true }
        super.setFrameSize(newSize)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    private func invalidateChipLayout() {
        chipLayout = nil
    }

    private var innerWidth: CGFloat { max(bounds.width - 2 * TransferCardMetrics.padding, 0) }

    private func currentChipLayout() -> NameChipLayout {
        if let chipLayout, chipLayoutWidth == innerWidth { return chipLayout }
        let made = NameChipLayout(chips: content.chips, labels: content.labels, width: innerWidth, maxLines: TransferCardMetrics.chipLines)
        chipLayout = made
        chipLayoutWidth = innerWidth
        return made
    }

    private var titleLines: Int {
        TransferCardMetrics.titleLines(naturalWidth: titleNaturalWidth, cardWidth: bounds.width)
    }

    /// Top of the tags (flipped coordinates).
    private var chipsOrigin: CGPoint {
        let metrics = TransferCardMetrics.self
        return CGPoint(x: metrics.padding, y: metrics.padding + CGFloat(titleLines) * metrics.titleLineHeight + metrics.groupSpacing)
    }

    // MARK: Drawing

    override func draw(_ dirtyRect: NSRect) {
        drawBackground()
        let metrics = TransferCardMetrics.self
        let x = metrics.padding
        let width = innerWidth
        let lines = titleLines
        drawKind(lineMidY: metrics.padding + metrics.titleLineHeight / 2)
        drawTitle(in: CGRect(x: x + metrics.titleIndent, y: metrics.padding, width: metrics.titleWidth(cardWidth: bounds.width),
                             height: CGFloat(lines) * metrics.titleLineHeight))
        var y = chipsOrigin.y

        let chips = currentChipLayout()
        if chips.lineCount > 0 {
            chips.draw(at: chipsOrigin)
            y += NameChipLayout.height(lines: chips.lineCount) + metrics.groupSpacing
        }

        drawProgress(in: CGRect(x: x, y: y, width: width, height: metrics.progressHeight))
        y += metrics.progressHeight + metrics.groupSpacing

        let line = metrics.lineHeight(metrics.infoFont)
        drawStatusLine(in: CGRect(x: x, y: y, width: width, height: line))
        y += line + metrics.infoLineSpacing
        let speeds = [content.downloadText, content.uploadText].filter { !$0.isEmpty }.joined(separator: "   ")
        drawInfoLine(left: speeds, right: content.trailingText, in: CGRect(x: x, y: y, width: width, height: line))
    }

    private func drawBackground() {
        let rect = bounds.insetBy(dx: 0.5, dy: 0.5)
        let path = NSBezierPath(roundedRect: rect, xRadius: TransferCardMetrics.cornerRadius, yRadius: TransferCardMetrics.cornerRadius)
        NSColor.quaternarySystemFill.setFill()
        path.fill()
        if isSelected {
            let accent = isEmphasized ? NSColor.selectedContentBackgroundColor : NSColor.unemphasizedSelectedContentBackgroundColor
            accent.withAlphaComponent(isEmphasized ? 0.14 : 0.5).setFill()
            path.fill()
            let ring = NSBezierPath(roundedRect: bounds.insetBy(dx: 1.5, dy: 1.5), xRadius: TransferCardMetrics.cornerRadius - 1,
                                    yRadius: TransferCardMetrics.cornerRadius - 1)
            ring.lineWidth = isEmphasized ? 3 : 2
            (isEmphasized ? NSColor.selectedContentBackgroundColor : NSColor.tertiaryLabelColor).setStroke()
            ring.stroke()
        } else {
            NSColor.separatorColor.setStroke()
            path.lineWidth = 1
            path.stroke()
        }
        if isMenuTarget {
            let ring = NSBezierPath(roundedRect: bounds.insetBy(dx: 1, dy: 1), xRadius: TransferCardMetrics.cornerRadius,
                                    yRadius: TransferCardMetrics.cornerRadius)
            ring.lineWidth = 2
            NSColor.keyboardFocusIndicatorColor.setStroke()
            ring.stroke()
        }
    }

    /// The content kind symbol, centered on the title's first line.
    private func drawKind(lineMidY: CGFloat) {
        let configuration = NSImage.SymbolConfiguration(pointSize: TransferCardMetrics.kindSymbolSize, weight: .medium)
            .applying(NSImage.SymbolConfiguration(paletteColors: [.secondaryLabelColor]))
        guard let image = SymbolImageCache.image(content.kind.symbol)?.withSymbolConfiguration(configuration) else { return }
        let box = TransferCardMetrics.titleIndent - 4
        let size = image.size
        image.draw(in: CGRect(x: TransferCardMetrics.padding + (box - size.width) / 2, y: lineMidY - size.height / 2,
                              width: size.width, height: size.height))
    }

    private static let titleParagraph: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byWordWrapping
        return style
    }()

    private static let tailParagraph: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    private func drawTitle(in rect: CGRect) {
        let attributes: [NSAttributedString.Key: Any] = [
            .font: TransferCardMetrics.titleFont, .foregroundColor: NSColor.labelColor, .paragraphStyle: Self.titleParagraph,
        ]
        (content.title as NSString).draw(with: rect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine], attributes: attributes)
    }

    private func drawProgress(in rect: CGRect) {
        let radius = rect.height / 2
        NSColor.quaternaryLabelColor.setFill()
        NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius).fill()
        let fraction = min(max(content.progress, 0), 1)
        guard fraction > 0 else { return }
        let filled = CGRect(x: rect.minX, y: rect.minY, width: max(rect.width * fraction, rect.height), height: rect.height)
        let tint = content.progressTint ?? .controlAccentColor
        (content.dimsProgress ? tint.withAlphaComponent(0.45) : tint).setFill()
        NSBezierPath(roundedRect: filled, xRadius: radius, yRadius: radius).fill()
    }

    private func infoAttributes(_ color: NSColor) -> [NSAttributedString.Key: Any] {
        [.font: TransferCardMetrics.infoFont, .foregroundColor: color, .paragraphStyle: Self.tailParagraph]
    }

    /// State icon, status and percent on the left; the size on the right.
    private func drawStatusLine(in rect: CGRect) {
        var left = rect
        let iconSize = TransferCardMetrics.iconSize
        if let image = Self.symbol(content.stateSymbol, color: content.stateColor) {
            image.draw(in: CGRect(x: rect.minX, y: rect.midY - iconSize / 2, width: iconSize, height: iconSize))
            left.origin.x += iconSize + TransferCardMetrics.iconGap
            left.size.width -= iconSize + TransferCardMetrics.iconGap
        }
        let status = NSMutableAttributedString(string: content.statusText,
                                               attributes: infoAttributes(content.statusTextColor ?? .secondaryLabelColor))
        if !content.percentText.isEmpty, content.progress < 1 {
            status.append(NSAttributedString(string: " · " + content.percentText, attributes: infoAttributes(.secondaryLabelColor)))
        }
        drawPair(left: status, right: content.sizeText, in: left)
    }

    private func drawInfoLine(left: String, right: String, in rect: CGRect) {
        drawPair(left: NSAttributedString(string: left, attributes: infoAttributes(.secondaryLabelColor)), right: right, in: rect)
    }

    /// `right` at full width (right-aligned), `left` truncated to the rest.
    private func drawPair(left: NSAttributedString, right: String, in rect: CGRect) {
        var rightWidth: CGFloat = 0
        if !right.isEmpty {
            let attributed = NSAttributedString(string: right, attributes: infoAttributes(.secondaryLabelColor))
            rightWidth = min(ceil(attributed.size().width), rect.width)
            attributed.draw(with: CGRect(x: rect.maxX - rightWidth, y: rect.minY, width: rightWidth, height: rect.height),
                            options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
        let leftWidth = rect.width - rightWidth - (rightWidth > 0 ? 8 : 0)
        guard leftWidth > 0, left.length > 0 else { return }
        left.draw(with: CGRect(x: rect.minX, y: rect.minY, width: leftWidth, height: rect.height),
                  options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private static func symbol(_ name: String, color: NSColor) -> NSImage? {
        guard !name.isEmpty else { return nil }
        let configuration = NSImage.SymbolConfiguration(pointSize: TransferCardMetrics.iconSize - 1, weight: .regular)
            .applying(NSImage.SymbolConfiguration(paletteColors: [color]))
        return SymbolImageCache.image(name)?.withSymbolConfiguration(configuration)
    }

    // MARK: Accessibility

    override func accessibilityLabel() -> String? {
        var parts = [content.kind.title, content.title]
        if !content.chips.isEmpty {
            parts.append(content.chips.map { "\($0.text) (\($0.category.title))" }.joined(separator: ", "))
        }
        parts += content.labels.map(\.name)
        parts.append(content.statusText)
        if content.progress < 1 { parts.append(content.percentText) }
        parts.append(content.sizeText)
        parts += [content.downloadText, content.uploadText, content.trailingText].filter { !$0.isEmpty }
        return parts.filter { !$0.isEmpty }.joined(separator: ", ")
    }

    override func accessibilityHelp() -> String? {
        content.name == content.title ? nil : content.name
    }

    override func isAccessibilitySelected() -> Bool { isSelected }

    override func accessibilityChildren() -> [Any]? {
        let layout = currentChipLayout()
        guard !layout.placed.isEmpty else { return nil }
        let origin = chipsOrigin
        let category = String(localized: "Category")
        let tag = String(localized: "cards.tag", defaultValue: "Tag", comment: "VoiceOver: a torrent tag shown on a card")
        let firstLabel = layout.placed.firstIndex { if case .label = $0.item { true } else { false } }
        return layout.placed.enumerated().map { index, placed in
            let element = NSAccessibilityElement()
            element.setAccessibilityRole(.staticText)
            element.setAccessibilityParent(self)
            switch placed.item {
            case .chip(let chip):
                element.setAccessibilityLabel("\(chip.text), \(chip.category.title)")
            case .label(let label):
                let isCategory = content.hasCategory && index == firstLabel
                element.setAccessibilityLabel("\(label.name), \(isCategory ? category : tag)")
            case nil:
                element.setAccessibilityLabel(String(localized: "cards.moreTags", defaultValue: "\(layout.hiddenCount) more tags",
                                                     comment: "VoiceOver: tags that did not fit on a card"))
            }
            // Frames in parent space are in the parent's (unflipped) coordinates.
            let frame = placed.frame.offsetBy(dx: origin.x, dy: origin.y)
            element.setAccessibilityFrameInParentSpace(CGRect(x: frame.minX, y: bounds.height - frame.maxY, width: frame.width, height: frame.height))
            return element
        }
    }
}
