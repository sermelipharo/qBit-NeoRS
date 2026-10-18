import AppKit

/// Places tags in rows and draws them: the name's tags in their category's
/// fixed style, then the torrent's category and tags as neutral tags with
/// their icon. Rows wrap up to `maxLines`; when the tags do not fit, the
/// name's last tags give way to a "+N" tag, so the category and tags stay
/// visible. Category symbols show when "Differentiate without color" is on.
@MainActor
struct NameChipLayout {
    enum Item: Equatable {
        case chip(NameChip)
        case label(TableCellContent.Label)

        var text: String {
            switch self {
            case .chip(let chip): chip.text
            case .label(let label): label.name
            }
        }
    }

    struct Placed: Equatable {
        /// Nil for the "+N" tag.
        let item: Item?
        let text: String
        let frame: CGRect
    }

    let placed: [Placed]
    /// Name tags that did not fit (counted in the "+N" tag).
    let hiddenCount: Int
    /// Rows used (0 without tags).
    let lineCount: Int

    private static var widths: [String: CGFloat] = [:]

    /// Lays out `chips` and `labels` from the top-left of a `width`-wide area (flipped coordinates).
    init(chips: [NameChip], labels: [TableCellContent.Label] = [], width: CGFloat, maxLines: Int,
         symbols: Bool = NameChipStyle.showsSymbols) {
        let (placed, hidden) = Self.layout(chips: chips, labels: labels.map(Item.label), width: width, maxLines: maxLines, symbols: symbols)
        self.init(placed: placed, hidden: hidden)
    }

    private static func layout(
        chips: [NameChip], labels: [Item], width: CGFloat, maxLines: Int, symbols: Bool
    ) -> (placed: [Placed], hidden: Int) {
        for kept in stride(from: chips.count, through: 0, by: -1) {
            let hidden = chips.count - kept
            if let placed = place(chips.prefix(kept).map(Item.chip), overflow: hidden, labels: labels, width: width,
                                  maxLines: maxLines, symbols: symbols) {
                return (placed, hidden)
            }
        }
        // Even the category and tags alone overflow: flow everything and cut at the last row.
        let everything = chips.map(Item.chip) + labels
        let placed = place(everything, overflow: 0, labels: [], width: width, maxLines: .max, symbols: symbols) ?? []
        let lastY = y(maxLines - 1)
        var visible = placed.filter { $0.frame.minY <= lastY }
        var hidden = placed.count - visible.count
        guard hidden > 0 else { return (visible, 0) }
        while true {
            let text = "+\(hidden)"
            let chipWidth = Self.width(of: text, symbol: false)
            let start = visible.last { $0.frame.minY == lastY }.map { $0.frame.maxX + NameChipStyle.spacing } ?? 0
            if start + chipWidth <= width || start == 0 {
                visible.append(Placed(item: nil, text: text, frame: CGRect(x: start, y: lastY, width: min(chipWidth, width), height: NameChipStyle.height)))
                return (visible, hidden)
            }
            visible.removeLast()
            hidden += 1
        }
    }

    private init(placed: [Placed], hidden: Int) {
        self.placed = placed
        hiddenCount = hidden
        lineCount = placed.map { Int(($0.frame.minY / (NameChipStyle.height + NameChipStyle.lineSpacing)).rounded()) + 1 }.max() ?? 0
    }

    /// Flows `items`, then a "+`overflow`" tag when it is not zero, then `labels`; nil when they need more than `maxLines` rows.
    private static func place(
        _ items: [Item], overflow: Int, labels: [Item], width: CGFloat, maxLines: Int, symbols: Bool
    ) -> [Placed]? {
        var entries: [(Item?, String)] = items.map { ($0, $0.text) }
        if overflow > 0 { entries.append((nil, "+\(overflow)")) }
        entries += labels.map { ($0, $0.text) }
        var placed: [Placed] = []
        var x: CGFloat = 0
        var line = 0
        for (item, text) in entries {
            let chipWidth = min(itemWidth(item, text: text, symbols: symbols), width)
            if x > 0, x + chipWidth > width {
                line += 1
                x = 0
            }
            if line >= maxLines { return nil }
            placed.append(Placed(item: item, text: text, frame: CGRect(x: x, y: y(line), width: chipWidth, height: NameChipStyle.height)))
            x += chipWidth + NameChipStyle.spacing
        }
        return placed
    }

    private static func itemWidth(_ item: Item?, text: String, symbols: Bool) -> CGFloat {
        switch item {
        case .chip: width(of: text, symbol: symbols)
        case .label(let label): width(of: text, symbol: !label.symbol.isEmpty)
        case nil: width(of: text, symbol: false)
        }
    }

    private static func y(_ line: Int) -> CGFloat {
        CGFloat(line) * (NameChipStyle.height + NameChipStyle.lineSpacing)
    }

    /// Height of `lines` rows of tags.
    static func height(lines: Int) -> CGFloat {
        guard lines > 0 else { return 0 }
        return CGFloat(lines) * NameChipStyle.height + CGFloat(lines - 1) * NameChipStyle.lineSpacing
    }

    static func width(of text: String, symbol: Bool) -> CGFloat {
        let textWidth: CGFloat
        if let cached = widths[text] {
            textWidth = cached
        } else {
            textWidth = ceil((text as NSString).size(withAttributes: [.font: NameChipStyle.font]).width)
            if widths.count > 2_048 { widths.removeAll(keepingCapacity: true) }
            widths[text] = textWidth
        }
        return textWidth + 2 * NameChipStyle.horizontalPadding + (symbol ? NameChipStyle.symbolSize + NameChipStyle.symbolGap : 0)
    }

    /// Draws the tags offset by `origin` in the current (flipped) graphics context.
    func draw(at origin: CGPoint, symbols: Bool = NameChipStyle.showsSymbols) {
        for entry in placed {
            let frame = entry.frame.offsetBy(dx: origin.x, dy: origin.y)
            let colors: NameChipStyle.Colors
            var symbol: NSImage?
            switch entry.item {
            case .chip(let chip):
                colors = NameChipStyle.colors(chip.category)
                if symbols { symbol = Self.symbolImage(NameChipStyle.symbol(chip.category), color: colors.text) }
            case .label(let label):
                colors = NameChipStyle.label
                symbol = Self.symbolImage(label.symbol, color: label.color)
            case nil:
                colors = NameChipStyle.overflow
            }
            let path = NSBezierPath(roundedRect: frame.insetBy(dx: 0.5, dy: 0.5), xRadius: NameChipStyle.cornerRadius,
                                    yRadius: NameChipStyle.cornerRadius)
            colors.fill.setFill()
            path.fill()
            if let border = colors.border {
                border.setStroke()
                path.lineWidth = 1
                path.stroke()
            }
            var textX = frame.minX + NameChipStyle.horizontalPadding
            if let symbol {
                let size = symbol.size
                symbol.draw(in: CGRect(x: textX + (NameChipStyle.symbolSize - size.width) / 2, y: frame.midY - size.height / 2,
                                       width: size.width, height: size.height))
                textX += NameChipStyle.symbolSize + NameChipStyle.symbolGap
            }
            let attributes: [NSAttributedString.Key: Any] = [
                .font: NameChipStyle.font, .foregroundColor: colors.text, .paragraphStyle: Self.truncating,
            ]
            let font = NameChipStyle.font
            let textHeight = ceil(font.ascender - font.descender)
            let textRect = CGRect(x: textX, y: frame.midY - textHeight / 2, width: frame.maxX - NameChipStyle.horizontalPadding - textX,
                                  height: textHeight)
            (entry.text as NSString).draw(with: textRect, options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine],
                                          attributes: attributes)
        }
    }

    private static let truncating: NSParagraphStyle = {
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byTruncatingTail
        return style
    }()

    private static func symbolImage(_ name: String, color: NSColor) -> NSImage? {
        guard !name.isEmpty else { return nil }
        let configuration = NSImage.SymbolConfiguration(pointSize: NameChipStyle.symbolSize, weight: .medium)
            .applying(NSImage.SymbolConfiguration(paletteColors: [color]))
        return SymbolImageCache.image(name)?.withSymbolConfiguration(configuration)
    }
}
