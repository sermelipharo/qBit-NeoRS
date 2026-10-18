import AppKit

/// Progress bar cell drawn directly (no subviews): a rounded track, the
/// filled part in the tint color and the centered percentage, white over the
/// filled part (qBittorrent's table progress delegate, as `ProgressBarView`).
@MainActor
final class TableProgressCell: NSTableCellView {
    struct Content: Equatable {
        var progress: Double
        /// Nil uses the accent color.
        var tint: NSColor?
        var isDimmed = false
    }

    private static let font = NSFont.monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize - 1, weight: .regular)
    private static let barHeight = CGFloat(Design.Size.tableProgressBarHeight)
    private static let radius = CGFloat(Design.Radius.small)
    private static let inset: CGFloat = 2

    private var content = Content(progress: 0)
    private var text = ""

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        setAccessibilityElement(true)
        setAccessibilityRole(.progressIndicator)
        setAccessibilityLabel(String(localized: "Progress"))
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView, identifier: NSUserInterfaceItemIdentifier) -> TableProgressCell {
        table.makeView(withIdentifier: identifier, owner: nil) as? TableProgressCell ?? TableProgressCell(identifier: identifier)
    }

    func configure(_ new: Content) {
        guard new != content || text.isEmpty else { return }
        content = new
        text = ValueFormatter.progress(new.progress)
        setAccessibilityValue(text)
        needsDisplay = true
    }

    override var backgroundStyle: NSView.BackgroundStyle {
        didSet { if backgroundStyle != oldValue { needsDisplay = true } }
    }

    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        let emphasized = backgroundStyle == .emphasized
        let bar = NSRect(x: Self.inset, y: ((bounds.height - Self.barHeight) / 2).rounded(),
                         width: max(bounds.width - 2 * Self.inset, 0), height: Self.barHeight)
        let track = NSBezierPath(roundedRect: bar, xRadius: Self.radius, yRadius: Self.radius)
        (emphasized ? NSColor.white.withAlphaComponent(0.2) : NSColor.quaternaryLabelColor).setFill()
        track.fill()

        let attributes: [NSAttributedString.Key: Any] = [
            .font: Self.font,
            .foregroundColor: emphasized ? NSColor.alternateSelectedControlTextColor : NSColor.secondaryLabelColor,
        ]
        let string = NSAttributedString(string: text, attributes: attributes)
        let size = string.size()
        let textOrigin = NSPoint(x: bar.midX - size.width / 2, y: bar.midY - size.height / 2)
        string.draw(at: textOrigin)

        let fraction = min(max(content.progress, 0), 1)
        guard fraction > 0 else { return }
        NSGraphicsContext.saveGraphicsState()
        NSBezierPath(rect: NSRect(x: bar.minX, y: bar.minY, width: bar.width * fraction, height: bar.height)).addClip()
        let fill: NSColor
        if content.isDimmed {
            fill = .tertiaryLabelColor
        } else if emphasized, content.tint == nil {
            fill = NSColor.white.withAlphaComponent(0.55)
        } else {
            fill = content.tint ?? .controlAccentColor
        }
        fill.setFill()
        track.fill()
        var white = attributes
        white[.foregroundColor] = NSColor.white
        NSAttributedString(string: text, attributes: white).draw(at: textOrigin)
        NSGraphicsContext.restoreGraphicsState()
    }
}
