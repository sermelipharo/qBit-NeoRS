import AppKit

/// One card, drawn in a single view (no subviews). Pinned to the top: the
/// content kind symbol, the title (semibold, one or two lines) and the state
/// symbol (large, in the top-right corner), then the name's tags followed by
/// the category and tags. Pinned to the bottom: the progress bar, the status
/// word (bold rounded) and percent with the ratio and size, and, only while
/// transferring, the speeds with the ETA. A card in a row with a taller one
/// keeps the extra height between the two blocks. Redraws only when its
/// content, selection or size changes. VoiceOver reads it as one element
/// with a summary, and each tag as a child with its category.
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
        let firstLineMidY = metrics.padding + metrics.titleLineHeight / 2
        drawKind(lineMidY: firstLineMidY)
        drawStateSymbol(lineMidY: firstLineMidY)
        drawTitle(in: CGRect(x: x + metrics.titleIndent, y: metrics.padding, width: metrics.titleWidth(cardWidth: bounds.width),
                             height: CGFloat(titleLines) * metrics.titleLineHeight))

        let chips = currentChipLayout()
        if chips.lineCount > 0 { chips.draw(at: chipsOrigin) }

        // The bottom block, from the card's bottom edge up.
        let infoLines = content.hasActivity ? 2 : 1
        var y = bounds.height - metrics.bottomBlockHeight(infoLines: infoLines)
        drawProgress(in: CGRect(x: x, y: y, width: width, height: metrics.progressHeight))
        y += metrics.progressHeight + metrics.groupSpacing

        let line = metrics.infoLineHeight
        drawStatusLine(in: CGRect(x: x, y: y, width: width, height: line))
        guard infoLines > 1 else { return }
        y += line + metrics.infoLineSpacing
        let speeds = [content.downloadText, content.uploadText].filter { !$0.isEmpty }.joined(separator: "   ")
        drawInfoLine(left: speeds, right: content.etaText, in: CGRect(x: x, y: y, width: width, height: line))
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

    /// The state symbol in the top-right corner, centered on the title's first
    /// line, always in the state color.
    private func drawStateSymbol(lineMidY: CGFloat) {
        let box = TransferCardMetrics.stateSymbolBox
        guard let image = Self.symbol(content.stateSymbol, color: content.stateColor) else { return }
        let scale = min(box / max(image.size.width, 1), box / max(image.size.height, 1), 1)
        let size = CGSize(width: image.size.width * scale, height: image.size.height * scale)
        let centerX = bounds.width - TransferCardMetrics.padding - box / 2
        image.draw(in: CGRect(x: centerX - size.width / 2, y: lineMidY - size.height / 2, width: size.width, height: size.height))
    }

    /// Fixed line height: a fallback font (an em dash, CJK) must not make two lines taller than the card allows.
    private static let titleParagraph: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byWordWrapping
        style.minimumLineHeight = TransferCardMetrics.titleLineHeight
        style.maximumLineHeight = TransferCardMetrics.titleLineHeight
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

    /// Status (bold rounded, in the state color when "Color by state" includes
    /// the status) and percent on the left; the ratio and size on the right.
    /// What does not fit gives way: the ratio first, then the percent (the bar
    /// shows it), then the status is truncated. VoiceOver reads them all.
    private func drawStatusLine(in rect: CGRect) {
        let status = NSMutableAttributedString(string: content.statusText, attributes: [
            .font: TransferCardMetrics.statusFont, .foregroundColor: content.statusTextColor ?? NSColor.labelColor,
            .paragraphStyle: Self.tailParagraph,
        ])
        let size = content.sizeText
        let sizeWidth = textWidth(size)
        var fits = true
        if !content.percentText.isEmpty, content.progress < 1 {
            let percent = NSAttributedString(string: " · " + content.percentText, attributes: infoAttributes(.secondaryLabelColor))
            fits = ceil(status.size().width) + ceil(percent.size().width) + Self.pairGap + sizeWidth <= rect.width
            if fits { status.append(percent) }
        }
        var right = size
        if fits, !content.ratioText.isEmpty {
            let full = content.ratioText + " · " + size
            if ceil(status.size().width) + Self.pairGap + textWidth(full) <= rect.width { right = full }
        }
        drawPair(left: status, right: right, in: rect)
    }

    private func textWidth(_ text: String) -> CGFloat {
        text.isEmpty ? 0 : ceil(NSAttributedString(string: text, attributes: infoAttributes(.secondaryLabelColor)).size().width)
    }

    private func drawInfoLine(left: String, right: String, in rect: CGRect) {
        drawPair(left: NSAttributedString(string: left, attributes: infoAttributes(.secondaryLabelColor)), right: right, in: rect)
    }

    private static let pairGap: CGFloat = 8

    /// `right` at full width (right-aligned), `left` truncated to the rest.
    private func drawPair(left: NSAttributedString, right: String, in rect: CGRect) {
        var rightWidth: CGFloat = 0
        if !right.isEmpty {
            let attributed = NSAttributedString(string: right, attributes: infoAttributes(.secondaryLabelColor))
            rightWidth = min(ceil(attributed.size().width), rect.width)
            attributed.draw(with: CGRect(x: rect.maxX - rightWidth, y: rect.minY, width: rightWidth, height: rect.height),
                            options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
        let leftWidth = rect.width - rightWidth - (rightWidth > 0 ? Self.pairGap : 0)
        guard leftWidth > 0, left.length > 0 else { return }
        left.draw(with: CGRect(x: rect.minX, y: rect.minY, width: leftWidth, height: rect.height),
                  options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
    }

    private static func symbol(_ name: String, color: NSColor) -> NSImage? {
        guard !name.isEmpty else { return nil }
        let configuration = NSImage.SymbolConfiguration(pointSize: TransferCardMetrics.stateSymbolBox - 2, weight: .regular)
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
        parts += [content.ratioText, content.sizeText, content.downloadText, content.uploadText, content.etaText]
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
