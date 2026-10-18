#if DEBUG
import AppKit

/// Live-run scenarios "live-palette", "live-menubar" and "live-lock" (see `liveSettings`): the Command Palette,
/// the menu bar item in each content mode, and quitting while locked and hidden.
extension UIScenarios {
    // MARK: Command Palette

    static func livePalette(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let palette = d.model.palette
        let main = try mainWindow(d)

        // Open with Cmd-K and inspect the panel: a normal-level child window over the main window, on its material.
        let panel = try await openPalette(d)
        d.check(panel.isVisible && panel.parent === main, "panel is a child window of the main window and visible")
        d.check(panel.level == main.level, "panel level \(panel.level.rawValue) equals the main window's (\(main.level.rawValue))")
        d.check(!panel.isFloatingPanel, "panel is not a floating panel")
        let material = panel.contentView as? NSVisualEffectView
        d.check(material?.material == .popover && material?.blendingMode == .behindWindow && material?.state == .active,
                "panel content view is a behind-window popover material (\(String(describing: material?.material.rawValue)))")
        d.check(panel.frame.intersects(main.frame) && main.frame.contains(CGPoint(x: panel.frame.midX, y: panel.frame.midY)),
                "panel is centered over the main window \(Int(panel.frame.midX)),\(Int(panel.frame.midY))")
        d.check(panel.alphaValue == 1, "panel alpha \(panel.alphaValue)")
        await d.pause(0.6)
        await d.shot("palette-open")

        // Typing filters; arrows move the selection.
        let allRows = palette.rows.count
        await typeInPalette(d, "turtle", panel)
        d.check(palette.rows.count < allRows && !palette.rows.isEmpty, "typing filtered the list \(allRows) -> \(palette.rows.count)")
        await d.shot("palette-filtered")
        await d.pause(0.2)
        let first = palette.selectedID
        await down(d, panel)
        let second = palette.selectedID
        await d.key("\u{F700}", code: 126, window: panel)
        d.check(first == palette.selectedID,
                "up arrow returned to the first row (\(first ?? "-") -> \(second ?? "-") -> \(palette.selectedID ?? "-"))")
        d.check(palette.selectedItem?.id == "command:alternativeSpeedLimits", "first match for 'turtle' is Alternative Speed Limits")

        // Return runs it.
        let before = d.model.alternativeSpeedLimits
        await d.pressReturn(window: panel)
        let toggled = await d.wait(timeout: 4) { d.model.alternativeSpeedLimits != before }
        d.check(toggled && !palette.isPresented,
                "Return toggled alternative speed limits \(before) -> \(d.model.alternativeSpeedLimits) and closed the palette")
        d.model.setAltSpeedLimits(before)
        await d.pause(0.5)

        // Tab on a torrent shows its actions.
        let panel2 = try await openPalette(d)
        guard let target = d.model.transfers.items.first else { throw UIDriver.Failure(description: "no torrents (use --demo)") }
        let words = target.name.split(separator: " ").prefix(2).joined(separator: " ")
        await typeInPalette(d, words, panel2)
        let row = "torrent:\(target.id)"
        var guardCount = 0
        while palette.selectedID != row, guardCount < 40 { await down(d, panel2); guardCount += 1 }
        await d.key("\t", code: 48, window: panel2)
        d.check(palette.mode == .torrent(target.id), "Tab showed the actions of '\(target.name)': \(palette.rows.map(\.title).prefix(5))")
        await d.pause(0.5)
        await d.shot("palette-torrent-actions")
        await d.pressEscape(window: panel2)
        d.check(palette.mode == .root && palette.isPresented, "Escape in the actions went back to the results, palette still open")
        await d.pressEscape(window: panel2)
        d.check(!palette.isPresented && palettePanel() == nil, "Escape closed the palette")

        // Cmd-K toggles.
        _ = try await openPalette(d)
        await d.command("k")
        d.check(!palette.isPresented, "Cmd-K again closed the palette")
        d.log("palette scenario done")
    }

    // MARK: Menu bar item

    static func statusWindow() -> NSWindow? {
        NSApp.windows.first { String(describing: type(of: $0)) == "NSStatusBarWindow" && $0.frame.minY > 0 && $0.isVisible }
    }

    static func liveMenuBar(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let window = try await openSettings(d)
        guard await openPane(d, window, .general) else { throw UIDriver.Failure(description: "General pane did not open") }
        await d.pause(0.8)
        let rowTitle = SettingsCatalog.title("Preferences/Desktop/MenuBarContentMode")
        guard let popup = control(d, window, titled: rowTitle) else { throw UIDriver.Failure(description: "content mode popup not found") }
        d.log("content mode popup: \(popup.description)")
        d.check(statusWindow() != nil, "status item window present: \(String(describing: statusWindow()?.frame))")

        // The item's image as drawn (light on the dark menu bar), for the four texts of this run.
        let samples = ["↓ 41M ↑ 12M", "↓  9K ↑  0", "13/32", "↓ 1.2M ↑340K  3/32"]
        let sheet = NSImage(size: NSSize(width: 360, height: 4 * 24 + 8), flipped: false) { rect in
            NSColor(white: 0.16, alpha: 1).setFill()
            rect.fill()
            for (index, sample) in samples.enumerated() {
                let image = MenuBarTextImage.make(sample, symbol: "arrow.up.arrow.down")
                let tinted = NSImage(size: image.size, flipped: false) { box in
                    image.draw(in: box)
                    NSColor.white.set()
                    box.fill(using: .sourceIn)
                    return true
                }
                tinted.draw(at: NSPoint(x: 8, y: 8 + CGFloat(index) * 24), from: .zero, operation: .sourceOver, fraction: 1)
            }
            return true
        }
        if let tiff = sheet.tiffRepresentation, let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) {
            let directory = ProcessInfo.processInfo.environment["QBT_UI_DIR"] ?? NSTemporaryDirectory()
            try? png.write(to: URL(filePath: directory + "/menubar-samples.png"))
        }

        for mode in MenuBarContentMode.allCases {
            // Open the real popup menu (accessibility press) and choose the item from a timer, since the menu
            // runs its own tracking loop.
            if d.model.gui.menuBarContentMode != mode {
                // Keys posted to the event queue reach the menu's tracking loop: arrows to the item, then Return.
                // The menu opens with no row highlighted, so the first Down lands on the first item.
                let steps = mode.rawValue + 1
                for index in 0 ..< steps {
                    d.afterCommon(1.0 + 0.25 * Double(index)) { d.postKey(.down) }
                }
                d.afterCommon(1.0 + 0.25 * Double(steps) + 0.3) { d.postKey(.returnKey) }
                d.press(popup.object)
                await d.pause(0.5)
            }
            let applied = await d.wait(timeout: 4) { d.model.gui.menuBarContentMode == mode }
            d.check(applied, "Settings popup set the menu bar content to '\(mode.title)' (now '\(d.model.gui.menuBarContentMode.title)')")
            await d.pause(1.2)
            // Width of the status item over ten seconds of changing demo speeds.
            var widths: [CGFloat] = []
            var pairs: [String] = []
            var shotTaken = false
            let end = Date().addingTimeInterval(10)
            while Date() < end {
                if let width = statusWindow()?.frame.width {
                    widths.append(width)
                    let gui = d.model.gui
                    let text = MenuBarTitle.speed(download: d.model.status.downloadRate, upload: d.model.status.uploadRate,
                                                  direction: gui.menuBarSpeedDirection, hidesZero: gui.menuBarHidesZeroSpeed) ?? ""
                    if !pairs.contains("\(width) '\(text)'") { pairs.append("\(width) '\(text)'") }
                }
                if !shotTaken, Date().addingTimeInterval(8) > end {
                    shotTaken = true
                    if let content = statusWindow()?.contentView {
                        let dump = d.elements(in: statusWindow()!).map(\.description).joined(separator: " | ")
                        d.log("status item views: \(dump)")
                        d.renderView(content, name: "menubar-\(mode.rawValue)-\(mode.title.replacingOccurrences(of: " ", with: "-"))")
                    }
                }
                await d.pause(0.02)
            }
            var changes: [String] = []
            for (index, width) in widths.enumerated() where index == 0 || width != widths[index - 1] { changes.append("#\(index):\(Int(width))") }
            d.log("width changes (20 ms samples): \(changes.prefix(40))")
            let distinct = Array(Set(widths)).sorted()
            d.log("width/text pairs: \(pairs.prefix(12))")
            let status = d.model.status
            d.log("mode \(mode.title): \(widths.count) width samples, distinct widths \(distinct), min \(distinct.first ?? 0) max \(distinct.last ?? 0); "
                + "rates ↓\(status.downloadRate) ↑\(status.uploadRate) torrents \(status.activeTorrents)/\(status.totalTorrents)")
            d.check(distinct.count <= 1, "status item width stayed constant in '\(mode.title)' (\(distinct))")
        }
        window.close()
    }

    // MARK: Lock and quit

    static func liveLock(_ d: UIDriver) async throws {
        try await waitRunning(d)
        try await lockThroughUI(d)
        try await hideThroughUI(d)

        d.log("--- quit with a WRONG password")
        scheduleQuitPromptAnswer(d, password: "not-the-password", shotPrefix: "livelock-wrong")
        scheduleInvalidPasswordDismissal(d, shotPrefix: "livelock-wrong")
        await d.command("q")
        await d.pause(14)
        d.check(NSApp.modalWindow == nil, "no modal alert left after the wrong password")
        d.check(d.model.lock.isLocked, "still locked and alive after the wrong password")
        if d.model.isMainWindowShown { try await hideThroughUI(d) }

        d.log("--- quit with the CORRECT password")
        scheduleQuitPromptAnswer(d, password: password, shotPrefix: "livelock-right")
        await d.command("q")
        await d.pause(30)
        d.log("FAIL: process still alive 30 s after the correct password")
    }
}
#endif
