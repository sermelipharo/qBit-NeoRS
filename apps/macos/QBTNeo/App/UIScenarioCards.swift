#if DEBUG
import AppKit

/// Scenario "cards": the transfer list and the cards view with parsed names.
/// Window captures of both modes (theme from `QBT_UI_THEME`), the cards'
/// selection sync, ⌘A, arrow keys, Return, the context menu, VoiceOver
/// labels, switching modes with the scroll position kept, a release and
/// rebuild of the main window (mode, selection and scroll restored), and the
/// Settings segment with the rules editor. Meant for a scratch `--profile`.
extension UIScenarios {
    static func cards(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let theme = ProcessInfo.processInfo.environment["QBT_UI_THEME"] ?? "light"
        NSApp.appearance = NSAppearance(named: theme == "dark" ? .darkAqua : .aqua)
        var window = try mainWindow(d)
        window.setContentSize(NSSize(width: 1_180, height: 780))
        d.model.gui.propertiesVisible = true
        d.model.gui.transferViewMode = .list
        _ = await d.wait(timeout: 30) { d.model.list.visibleItems.count >= 10 }
        await d.pause(2)
        let items = d.model.list.visibleItems
        d.log(String(format: "MEM list=%.1fMB torrents=%d", footprint(), items.count))
        d.model.list.selection = [items[1].id]
        await d.pause(1.5)
        await d.shot("list-\(theme)")

        // Cards
        d.model.gui.transferViewMode = .cards
        await d.pause(2)
        d.log(String(format: "MEM cards=%.1fMB", footprint()))
        d.log("card views created: \(TransferCardItem.loadedViews)")
        guard let collection = d.views(in: window, classNameContains: "TransferCardsCollectionView").first as? NSCollectionView else {
            throw UIDriver.Failure(description: "no cards view")
        }
        d.check(d.views(in: window, classNameContains: "NativeTableView").allSatisfy { $0.identifier?.rawValue != "TransferList" },
                "the list's table is gone in cards mode")
        d.check(collection.selectionIndexPaths == [IndexPath(item: 1, section: 0)], "selection carried over to cards")
        await d.shot("cards-\(theme)")
        d.model.gui.propertiesVisible = false
        await d.pause(1.5)
        await d.shot("cards-full-\(theme)")

        let visible = collection.visibleItems().compactMap { $0 as? TransferCardItem }
        let label = visible.first?.card.accessibilityLabel() ?? ""
        d.log("card VoiceOver label: \(label)")
        d.check(NameRuleCategory.allCases.contains { label.contains($0.title) }, "card label speaks tag categories")
        let chipLabels = (visible.first?.card.accessibilityChildren() as? [NSAccessibilityElement])?.compactMap { $0.accessibilityLabel() } ?? []
        d.log("chip labels: \(chipLabels.joined(separator: " | "))")

        // Keyboard: ⌘A, arrows, Return.
        window.makeFirstResponder(collection)
        await d.ensureKey(window)
        await d.command("a")
        await d.pause(0.5)
        if d.model.list.selection.count != items.count {
            // An inactive app has no key window, so the responder chain is empty: call what Select All calls.
            let target = NSApp.target(forAction: #selector(NSText.selectAll(_:)), to: nil, from: nil)
            d.log("⌘A did not arrive; Select All goes to \(String(describing: target.map { type(of: $0) }))")
            collection.selectAll(nil)
            await d.pause(0.5)
        }
        d.log("first responder \(String(describing: window.firstResponder.map { type(of: $0) })), cards selected \(collection.selectionIndexPaths.count)")
        d.check(d.model.list.selection.count == items.count, "⌘A selects every card (\(d.model.list.selection.count)/\(items.count))")
        d.model.list.selection = [items[0].id]
        await d.pause(0.5)
        d.keyNow(String(UnicodeScalar(NSRightArrowFunctionKey).map(Character.init) ?? " "), code: 124, window: window)
        await d.pause(0.5)
        d.check(d.model.list.selection == [items[1].id], "right arrow moves the selection to the next card")
        if let stopped = d.model.list.visibleItems.firstIndex(where: { $0.state == .stoppedDownloading }) {
            let id = d.model.list.visibleItems[stopped].id
            d.model.list.selection = [id]
            await d.pause(0.5)
            d.keyNow("\r", code: 36, window: window)
            let started = await d.wait(timeout: 8) { d.model.transfers.item(id)?.state != .stoppedDownloading }
            d.check(started, "Return runs the double-click action (starts a stopped torrent)")
        }

        // Context menu for a card.
        if let first = collection.item(at: IndexPath(item: 0, section: 0)) {
            let center = first.view.convert(NSPoint(x: first.view.bounds.midX, y: first.view.bounds.midY), to: nil)
            if let event = d.mouse(.rightMouseDown, at: center, in: window), let menu = collection.menu(for: event) {
                d.log("card menu: \(UIDriver.menuSummary(menu))")
                d.check(menu.items.contains { $0.title == String(localized: "Remove") }, "card context menu has the list's commands")
                collection.didCloseMenu(menu, with: nil)
            } else {
                d.check(false, "card context menu")
            }
        }

        // Switching modes keeps the scroll position (by the top torrent).
        d.model.list.selection = [items[items.count - 1].id]
        await d.pause(1)
        let scrolledY = collection.enclosingScrollView?.contentView.bounds.minY ?? 0
        d.model.gui.transferViewMode = .list
        await d.pause(1.5)
        if let table = try? transferTable(d) {
            let row = table.row(at: NSPoint(x: 10, y: table.enclosingScrollView?.contentView.bounds.midY ?? 0))
            d.check(row > items.count / 2, "list scrolled near the selected card after switching (row \(row), cards y \(Int(scrolledY)))")
        }
        d.model.gui.transferViewMode = .cards
        await d.pause(1.5)

        // Release and rebuild the main window: mode, selection and scroll come back.
        let selected = d.model.list.selection
        let before = (d.views(in: window, classNameContains: "TransferCardsCollectionView").first as? NSCollectionView)?
            .enclosingScrollView?.contentView.bounds.minY ?? -1
        window.close()
        await d.pause(1)
        NotificationCenter.default.post(name: MemoryPressureMonitor.warningNotification, object: nil)
        await d.pause(3)
        d.check(d.views(in: window, classNameContains: "TransferCardsCollectionView").isEmpty, "closed main window released its cards")
        d.model.openWindow?(id: AppWindowID.main.rawValue)
        _ = await d.wait(timeout: 10) { d.model.mainWindow?.isVisible == true }
        await d.pause(3)
        window = try mainWindow(d)
        let rebuilt = d.views(in: window, classNameContains: "TransferCardsCollectionView").first as? NSCollectionView
        d.check(rebuilt != nil, "rebuilt window shows cards again")
        d.check(d.model.list.selection == selected, "selection survived the release")
        let after = rebuilt?.enclosingScrollView?.contentView.bounds.minY ?? -2
        d.check(abs(after - before) < 4, "scroll position survived the release (\(Int(before)) → \(Int(after)))")
        d.log(String(format: "MEM after-rebuild=%.1fMB", footprint()))

        // Settings › Interface › Torrent Names and the rules editor.
        d.model.pendingSettingReveal = "GUI/TorrentNames/Rules"
        let settings = try await openSettings(d)
        await d.pause(2)
        await d.shot("settings-names-\(theme)")
        let editTitle = String(localized: "settings.names.editRules", defaultValue: "Edit Rules…", comment: "Settings: opens the name rules editor")
        if let edit = d.find(role: "AXButton", labelContains: editTitle).first {
            _ = await d.activate(edit) { settings.attachedSheet != nil }
            await d.pause(2)
            await d.shot("rules-editor-\(theme)")
            if let sheet = settings.attachedSheet { settings.endSheet(sheet) }
        } else {
            d.check(false, "Edit Rules button found")
        }
        settings.close()
        d.model.gui.transferViewMode = .list
        d.model.gui.propertiesVisible = true
        await d.pause(1)
        NSApp.appearance = nil
    }
}
#endif
