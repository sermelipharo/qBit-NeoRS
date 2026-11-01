#if DEBUG
import AppKit

/// Live-run scenario "live-settings-toolbar": switching Settings panes never draws a frame whose toolbar lacks
/// the search field. SwiftUI rebuilds the pane items on every switch, inside the display cycle, and drops the
/// field; the pane buttons then jump into its place. Each time it goes, it must come back within the same
/// display cycle, before that frame is drawn. Then, on every pane, the expanded search field keeps clear of
/// the pane buttons (they move aside) and they return when it collapses.
extension UIScenarios {
    static func liveSettingsToolbar(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let window = try await openSettings(d)
        guard let toolbar = window.toolbar else { throw UIDriver.Failure(description: "Settings has no toolbar") }
        let search = SettingsToolbarSearch.Coordinator.identifier
        nonisolated(unsafe) var removals = 0
        nonisolated(unsafe) var missing = 0
        nonisolated(unsafe) var pending = false
        // Inside the display cycle's layout (the frame not yet drawn) or not.
        @Sendable func inDisplayCycle() -> Bool { Thread.callStackSymbols.contains { $0.contains("NSDisplayCycleFlush") } }
        let center = NotificationCenter.default
        let removed = center.addObserver(forName: NSToolbar.didRemoveItemNotification, object: toolbar, queue: nil) { note in
            nonisolated(unsafe) let note = note
            MainActor.assumeIsolated {
                guard (note.userInfo?["item"] as? NSToolbarItem)?.itemIdentifier == search else { return }
                removals += 1
                // Outside the display cycle (layout forced by an event) nothing is drawn before it is back.
                pending = inDisplayCycle()
            }
        }
        let added = center.addObserver(forName: NSToolbar.willAddItemNotification, object: toolbar, queue: nil) { note in
            nonisolated(unsafe) let note = note
            MainActor.assumeIsolated {
                guard (note.userInfo?["item"] as? NSToolbarItem)?.itemIdentifier == search, pending else { return }
                pending = false
                if !inDisplayCycle() { missing += 1 }
            }
        }
        defer {
            center.removeObserver(removed)
            center.removeObserver(added)
        }
        for round in 0..<3 {
            for pane in SettingsPane.allCases {
                let ok = await openPane(d, window, pane)
                if !ok { d.check(false, "round \(round): pane '\(pane.title)' did not open") }
                await d.pause(0.4)
            }
        }
        d.log("search field removals \(removals), put back after the frame was drawn \(missing)")
        d.check(removals > 0, "switching panes rebuilt the toolbar (the case this guards)")
        d.check(missing == 0, "no frame drawn without the toolbar search field while switching panes (\(missing) of \(removals))")
        d.check(toolbar.items.last?.itemIdentifier == search, "the search field ends the toolbar")
        await expandedSearchClearsPanes(d, window, toolbar)
    }

    private static func rightmostButton(_ d: UIDriver, _ window: NSWindow) -> CGFloat? {
        d.elements(in: window, includingFrame: true).filter { element in
            SettingsPane.allCases.contains { $0.title == element.label } && element.frame.width > 0
        }.map(\.frame.maxX).max()
    }

    /// The expanded search field keeps clear of the pane buttons, on every pane.
    private static func expandedSearchClearsPanes(_ d: UIDriver, _ window: NSWindow, _ toolbar: NSToolbar) async {
        let search = SettingsToolbarSearch.Coordinator.identifier
        guard let item = toolbar.items.first(where: { $0.itemIdentifier == search }) as? NSSearchToolbarItem else {
            d.check(false, "no search item")
            return
        }
        for pane in SettingsPane.allCases {
            _ = await openPane(d, window, pane)
            await d.pause(0.4)
            func buttonsStart() -> Int {
                Int(d.elements(in: window, includingFrame: true).filter { element in
                    SettingsPane.allCases.contains { $0.title == element.label } && element.frame.width > 0
                        && !["AXStaticText", "AXImage"].contains(element.role)
                }.map(\.frame.minX).min() ?? 0)
            }
            let collapsedStart = buttonsStart()
            // What a click on the magnifying glass and Cmd-F do.
            item.beginSearchInteraction()
            var path = [collapsedStart]
            for _ in 0..<6 {
                try? await Task.sleep(for: .milliseconds(50))
                path.append(buttonsStart())
            }
            await d.pause(0.3)
            d.log("\(pane.rawValue): pane buttons start while expanding " + path.map(String.init).joined(separator: " "))
            let field = item.searchField.convert(item.searchField.bounds, to: nil)
            let buttons = d.elements(in: window, includingFrame: true).filter { element in
                SettingsPane.allCases.contains { $0.title == element.label } && element.frame.width > 0
                    && !["AXStaticText", "AXImage"].contains(element.role)
            }
            // Accessibility frames are in screen coordinates.
            let screenField = window.convertToScreen(field)
            let overlapping = buttons.filter { $0.frame.intersects(screenField) }.map(\.label)
            let rightmost = buttons.map(\.frame.maxX).max() ?? 0
            let starts = buttons.map(\.frame.minX).min() ?? 0
            d.log("\(pane.rawValue): window \(Int(window.frame.minX))..\(Int(window.frame.maxX)), "
                + "buttons \(Int(starts))..\(Int(rightmost)) (\(buttons.count)), field \(Int(screenField.minX))..\(Int(screenField.maxX))")
            let fits = overlapping.isEmpty && field.width > 100 && screenField.maxX <= window.frame.maxX - 8
            d.check(fits, "\(pane.title): the expanded search field (\(Int(field.width)) pt) clears the pane buttons \(overlapping) inside the window")
            window.makeFirstResponder(nil)
            await d.pause(0.6)
            let back = buttonsStart()
            d.check(abs(back - collapsedStart) <= 1, "\(pane.title): the pane buttons return when the empty field collapses (\(collapsedStart) -> \(back))")
        }
        // An accessibility press on the magnifying glass (VoiceOver), logged only.
        if let magnifier = d.elements(in: window, includingFrame: true).first(where: {
            $0.role == "AXButton" && $0.label == String(localized: "Search") && $0.frame.minX > (rightmostButton(d, window) ?? 0)
        }) {
            d.press(magnifier.object)
            await d.pause(0.6)
            let focused = item.searchField.currentEditor() != nil
            window.makeFirstResponder(nil)
            await d.pause(0.6)
            d.log("accessibility press: field focused \(focused), width after leaving \(Int(item.searchField.frame.width))")
        }
    }
}
#endif
