#if DEBUG
import AppKit

/// Scenario "cards-shots": window captures of the list and the cards with
/// the toolbar's List / Cards switch (theme from `QBT_UI_THEME`), the
/// toolbar items' frames (to measure what the switch moved), the switch's
/// segments, tooltips and sync with the setting, the toolbar in a narrow
/// window, card heights, and one rendered card per content kind in light
/// and dark (`card-<kind>-<theme>.png` in `QBT_UI_DIR`). For a scratch
/// `--profile` with torrents of many kinds.
extension UIScenarios {
    static func cardsShots(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let theme = ProcessInfo.processInfo.environment["QBT_UI_THEME"] ?? "light"
        NSApp.appearance = NSAppearance(named: theme == "dark" ? .darkAqua : .aqua)
        let window = try mainWindow(d)
        window.setContentSize(NSSize(width: 1_180, height: 780))
        d.model.gui.propertiesVisible = true
        d.model.gui.transferViewMode = .list
        _ = await d.wait(timeout: 30) { d.model.list.visibleItems.count >= 10 }
        await d.pause(2)
        logToolbar(d, window, "wide")
        await d.shot("list-\(theme)")

        // The switch: two segments with labels for VoiceOver, in sync with the setting.
        let titlebar = window.frame.maxY - (window.frame.height - window.contentLayoutRect.height)
        func segments() -> [UIDriver.Element] {
            let all = d.elements(in: window, includingFrame: true)
            return TransferViewMode.allCases.compactMap { mode in all.first { $0.label == mode.shortTitle && $0.frame.minY >= titlebar - 2 } }
        }
        let found = segments()
        d.log("picker AX: " + found.map(\.description).joined(separator: " | "))
        let helps = found.map { ($0.object.value(forKey: "accessibilityHelp") as? String) ?? "-" }
        d.log("picker help (tooltips): \(helps)")
        d.check(helps == TransferViewMode.allCases.map(TransferViewModePicker.tooltip), "each segment has its tooltip")
        if found.count != 2 {
            let toolbar = d.elements(in: window, includingFrame: true).filter { $0.frame.minY >= titlebar - 2 && !$0.label.isEmpty }
            d.log("toolbar AX: " + toolbar.map(\.description).joined(separator: " | "))
        }
        d.check(found.count == 2, "toolbar has List and Cards segments with VoiceOver labels")
        let before = found.map(\.value)
        d.model.gui.transferViewMode = .cards
        await d.pause(1.5)
        let after = segments().map(\.value)
        d.log("picker values list=\(before) cards=\(after)")
        d.check(before != after, "switch follows the setting (View menu, ⌘2)")
        if let list = segments().first {
            _ = await d.activate(list) { d.model.gui.transferViewMode == .list }
            d.check(d.model.gui.transferViewMode == .list, "pressing the List segment shows the list")
        }
        for view in d.views(in: window, classNameContains: "") where view.toolTip?.contains("⌘") == true {
            d.log("tooltip: \(type(of: view)) '\(view.toolTip ?? "")'")
        }

        d.model.gui.transferViewMode = .cards
        await d.pause(2)
        logToolbar(d, window, "cards")
        logCards(d, window)
        await d.shot("cards-\(theme)")
        d.model.gui.propertiesVisible = false
        await d.pause(1.5)
        logCards(d, window)
        await d.shot("cards-full-\(theme)")
        if let card = (d.views(in: window, classNameContains: "TransferCardView").first as? TransferCardView) {
            d.log("card VoiceOver label: \(card.accessibilityLabel() ?? "")")
        }
        renderKindCards(d, width: 330)

        // Narrow window: what overflows.
        let wide = window.frame
        window.setContentSize(NSSize(width: Design.Window.mainMinSize.width, height: 780))
        await d.pause(2)
        logToolbar(d, window, "narrow")
        logCards(d, window)
        await d.shot("narrow-\(theme)")
        window.setFrame(wide, display: true)
        d.model.gui.propertiesVisible = true
        d.model.gui.transferViewMode = .list
        await d.pause(1)
        NSApp.appearance = nil
    }

    /// Frames (window points from the top-left) of the toolbar's controls and which items overflow.
    private static func logToolbar(_ d: UIDriver, _ window: NSWindow, _ label: String) {
        let top = window.frame.height
        let titlebarBottom = window.contentLayoutRect.maxY
        var entries: [String] = []
        for view in d.views(in: window, classNameContains: "") where view.window != nil && !view.isHiddenOrHasHiddenAncestor {
            guard view is NSButton || view is NSSegmentedControl || view is NSSearchField else { continue }
            let rect = view.convert(view.bounds, to: nil)
            guard rect.minY >= titlebarBottom - 2, rect.width > 4 else { continue }
            let name = view.accessibilityLabel() ?? view.toolTip ?? (view as? NSButton)?.title ?? ""
            entries.append(String(format: "%@ '%@' x=%.1f w=%.1f y=%.1f h=%.1f", String(describing: type(of: view)), name,
                                  rect.minX, rect.width, top - rect.maxY, rect.height))
        }
        d.log("TOOLBAR \(label) window=\(Int(window.frame.width)): " + entries.sorted().joined(separator: " ; "))
        if let toolbar = window.toolbar {
            let visible = Set((toolbar.visibleItems ?? []).map(\.itemIdentifier))
            let hidden = toolbar.items.filter { !visible.contains($0.itemIdentifier) }.map { "\($0.itemIdentifier.rawValue) '\($0.label)'" }
            d.log("TOOLBAR \(label) items=\(toolbar.items.count) visible=\(visible.count) overflow: \(hidden.joined(separator: ", "))")
        }
    }

    /// Heights of the realized cards and their kinds.
    private static func logCards(_ d: UIDriver, _ window: NSWindow) {
        let cards = d.views(in: window, classNameContains: "TransferCardView").compactMap { $0 as? TransferCardView }
        let rows = Dictionary(grouping: cards) { Int($0.convert($0.bounds, to: nil).maxY) }
        let summary = rows.keys.sorted(by: >).map { key in
            (rows[key] ?? []).map { "\($0.content.kind.rawValue)·\(Int($0.frame.width))x\(Int($0.frame.height))" }.joined(separator: " ")
        }
        d.log("CARDS \(cards.count): " + summary.joined(separator: " | "))
        if let collection = d.views(in: window, classNameContains: "TransferCardsCollectionView").first as? NSCollectionView,
           let layout = collection.collectionViewLayout as? TransferCardsLayout {
            d.log("layout columns=\(layout.columnCount) cardWidth=\(layout.cardWidth) contentWidth=\(layout.contentWidth) "
                + "collection=\(collection.frame.width) clip=\(collection.enclosingScrollView?.contentView.bounds.width ?? 0)")
        }
    }

    /// One card per content kind, drawn off screen at 2x on the window background, light and dark.
    private static func renderKindCards(_ d: UIDriver, width: CGFloat) {
        let builder = TransferCardBuilder(gui: d.model.gui, labels: d.model.labels, names: d.model.names)
        var seen = Set<ContentKind>()
        for item in d.model.list.visibleItems {
            let content = builder.content(item)
            guard seen.insert(content.kind).inserted else { continue }
            let heights = TransferCardHeights()
            heights.update(item.id, input: builder.layoutInput(item))
            let height = heights.rowHeight(at: 0, ids: [item.id], columns: 1, cardWidth: width) { _ in builder.layoutInput(item) }
            for name in [NSAppearance.Name.aqua, .darkAqua] {
                let file = "\(d.directory)/card-\(content.kind.rawValue)-\(name == .aqua ? "light" : "dark").png"
                renderCard(content, size: NSSize(width: width, height: height), appearance: NSAppearance(named: name), to: file)
            }
            d.log("CARD \(content.kind.rawValue) \(Int(width))x\(Int(height)): \(item.name)")
        }
        d.log("kinds rendered: \(seen.map(\.rawValue).sorted().joined(separator: ", "))")
    }

    private static func renderCard(_ content: TransferCardContent, size: NSSize, appearance: NSAppearance?, to path: String) {
        let margin: CGFloat = 12
        let container = NSBox(frame: NSRect(x: 0, y: 0, width: size.width + 2 * margin, height: size.height + 2 * margin))
        container.boxType = .custom
        container.borderWidth = 0
        container.fillColor = .controlBackgroundColor
        container.contentViewMargins = .zero
        container.appearance = appearance
        let card = TransferCardView(frame: NSRect(origin: NSPoint(x: margin, y: margin), size: size))
        card.content = content
        container.addSubview(card)
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(container.bounds.width * 2), pixelsHigh: Int(container.bounds.height * 2),
                                         bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                         bytesPerRow: 0, bitsPerPixel: 0) else { return }
        rep.size = container.bounds.size
        appearance?.performAsCurrentDrawingAppearance {
            container.cacheDisplay(in: container.bounds, to: rep)
        }
        try? rep.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: path))
    }
}
#endif
