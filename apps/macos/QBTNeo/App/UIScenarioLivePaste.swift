#if DEBUG
import AppKit

/// Live-run scenario "live-paste" (use with `--demo`): ⌘V after a click in the Trackers tab adds the copied
/// trackers to the torrent, in new tiers; ⌘V in the transfer list adds copied trackers to the selected torrent
/// and opens an Add Torrent window for a copied magnet link, in front of the main window. Reads a private
/// pasteboard, so the user's clipboard is left alone.
extension UIScenarios {
    static func livePaste(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let window = try mainWindow(d)
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("qbtneo-ui-paste"))
        d.model.pasteboard = pasteboard
        defer {
            pasteboard.releaseGlobally()
            d.model.pasteboard = .general
        }
        func copy(_ text: String) {
            pasteboard.clearContents()
            pasteboard.setString(text, forType: .string)
        }
        // An inactive app has no key window, so ⌘V never reaches the menu: the Edit menu's paste: goes
        // straight into the responder chain of the focused view instead.
        func paste() {
            let handled = window.firstResponder?.tryToPerform(#selector(NSText.paste(_:)), with: nil) ?? false
            d.log("paste: handled \(handled) by the chain of \(window.firstResponder.map { String(describing: type(of: $0)) } ?? "none")")
        }

        guard let item = d.model.list.visibleItems.first else { throw UIDriver.Failure(description: "no torrents") }
        try await selectTorrent(d, named: item.name)
        d.model.gui.propertiesVisible = true
        d.model.gui.propertiesTab = .trackers
        _ = await d.wait(timeout: 5) { !d.model.properties.trackerEntries.isEmpty }
        await d.pause(1)

        // The Trackers tab: a click in the list, then ⌘V.
        let trackerTable = d.views(in: window, classNameContains: "TableView").compactMap { $0 as? NSTableView }
            .first { $0.identifier?.rawValue == "TrackerList" }
        guard let trackerTable else { throw UIDriver.Failure(description: "tracker list not found") }
        let before = d.model.properties.trackerEntries
        let lastTier = before.map(\.tier).max() ?? -1
        await d.clickRow(trackerTable, row: max(0, trackerTable.numberOfRows - 1))
        // Synthesized clicks don't move the focus of an inactive app.
        if window.firstResponder !== trackerTable { window.makeFirstResponder(trackerTable) }
        d.check(window.firstResponder === trackerTable, "the tracker list has focus")
        copy("udp://paste-one.example:1337/announce\n\nhttps://paste-two.example/announce\n")
        paste()
        let added = await d.wait(timeout: 6) { d.model.properties.trackerEntries.count == before.count + 2 }
        let entries = d.model.properties.trackerEntries
        d.check(added, "⌘V in Trackers added 2 trackers (\(before.count) → \(entries.count))")
        let one = entries.first { $0.url.contains("paste-one") }
        let two = entries.first { $0.url.contains("paste-two") }
        d.check(one?.tier == lastTier + 1 && two?.tier == lastTier + 2,
                "pasted tiers follow the last one (\(lastTier) → \(one?.tier ?? -9), \(two?.tier ?? -9))")
        await d.shot("paste-trackers")

        // The transfer list: trackers go to the selected torrent.
        try await selectTorrent(d, named: item.name)
        let transfers = try transferTable(d)
        if window.firstResponder !== transfers { window.makeFirstResponder(transfers) }
        d.check(window.firstResponder === transfers, "the transfer list has focus")
        let count = d.model.properties.trackerEntries.count
        copy("https://paste-three.example/announce")
        paste()
        let toSelected = await d.wait(timeout: 6) {
            d.model.properties.trackerEntries.contains { $0.url.contains("paste-three") }
        }
        d.check(toSelected, "⌘V in the transfer list added the tracker to the selected torrent (\(count) → \(d.model.properties.trackerEntries.count))")

        // "Add trackers": Download appends the list at the URL to the editor.
        d.model.gui.trackersListURL = "https://lists.example.org/trackers_best.txt"
        d.model.sheet = .addTrackers(id: item.id)
        let download = try await d.require(role: "AXButton", label: "Download", timeout: 10)
        await d.click(download)
        let filled = await d.wait(timeout: 8) {
            d.find(role: "AXTextArea").contains { $0.value.contains("tracker.opentrackr.org") }
        }
        d.check(filled, "Download filled the Add trackers editor")
        await d.shot("add-trackers-download")
        if let cancel = d.find(role: "AXButton", label: "Cancel").first { await d.click(cancel) }
        _ = await d.wait(timeout: 5) { d.model.sheet == nil }
        window.makeFirstResponder(transfers)

        // A magnet link opens the Add Torrent window in front of the main window.
        let dialogs = d.model.addDialogs.count
        copy("magnet:?xt=urn:btih:\(String(repeating: "c0ffee", count: 6))c0fe&dn=Paste+test")
        paste()
        let opened = await d.wait(timeout: 6) { d.model.addDialogs.count == dialogs + 1 }
        d.check(opened, "⌘V with a magnet link opened an Add Torrent dialog")
        await d.pause(1.5)
        let front = NSApp.orderedWindows.first { $0.isVisible }
        d.log("front window: '\(front?.title ?? "none")'")
        d.check(front !== window && front?.identifier?.rawValue.hasPrefix(AppWindowID.addTorrent.rawValue) == true,
                "the Add Torrent window is in front of the main window")
        await d.shot("paste-magnet")
        if let cancel = d.find(role: "AXButton", label: "Cancel").first { await d.click(cancel) }
    }
}
#endif
