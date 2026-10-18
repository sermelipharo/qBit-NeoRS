#if DEBUG
import AppKit

/// Live-run scenario "live-settings": the Settings window driven like a user — ⌘, toolbar panes, ⌘F search
/// (Return and click on a result, Escape), "Show advanced settings", persistence of a changed setting and the
/// Test Port button. Meant for `QBT_UI_VISIBLE=1` runs with a scratch profile; languages via `-AppleLanguages`.
extension UIScenarios {
    // MARK: Settings window helpers

    static func settingsWindow(_ d: UIDriver) -> NSWindow? {
        NSApp.windows.first { window in
            window.isVisible && window.toolbar?.items.contains { $0.itemIdentifier.rawValue == "SettingsSearch" } == true
        }
    }

    static func settingsSearchField(_ window: NSWindow) -> NSSearchField? {
        (window.toolbar?.items.first { $0.itemIdentifier.rawValue == "SettingsSearch" } as? NSSearchToolbarItem)?.searchField
    }

    static func openSettings(_ d: UIDriver) async throws -> NSWindow {
        if let existing = settingsWindow(d) { return existing }
        await d.command(",", code: 43)
        _ = await d.wait(timeout: 15) { settingsWindow(d) != nil }
        guard let window = settingsWindow(d) else { throw UIDriver.Failure(description: "Settings did not open on Cmd-,") }
        await d.pause(1)
        return window
    }

    /// The toolbar button of a pane (found through the accessibility tree of the title bar).
    static func paneButton(_ d: UIDriver, _ window: NSWindow, _ pane: SettingsPane) -> UIDriver.Element? {
        let candidates = d.elements(in: window, includingFrame: true).filter {
            $0.label == pane.title && $0.frame.width > 0 && !["AXStaticText", "AXImage"].contains($0.role)
        }
        return candidates.first { $0.role == "AXRadioButton" } ?? candidates.first { $0.role == "AXButton" } ?? candidates.first
    }

    static func openPane(_ d: UIDriver, _ window: NSWindow, _ pane: SettingsPane) async -> Bool {
        if window.title == pane.title { return true }
        guard let button = paneButton(d, window, pane) else {
            d.log("FAIL: no toolbar button for pane '\(pane.title)'")
            let labels = d.elements(in: window, includingFrame: true).filter { !$0.label.isEmpty }.prefix(40)
            for item in labels { d.log("  toolbar candidate \(item.role) '\(item.label)' \(item.frame)") }
            return false
        }
        return await d.activate(button) { window.title == pane.title }
    }

    static func texts(_ d: UIDriver, _ window: NSWindow) -> [String] {
        d.elements(in: window).flatMap { [$0.value, $0.label] }.filter { !$0.isEmpty }
    }

    static func paneScrollOffset(_ d: UIDriver, _ window: NSWindow) -> CGFloat {
        let scrolls = d.views(in: window, classNameContains: "ScrollView").compactMap { $0 as? NSScrollView }
        return scrolls.max(by: { $0.frame.height < $1.frame.height })?.contentView.bounds.origin.y ?? -1
    }

    /// The control of a row titled `title`: an element labelled with it (a checkbox) or the control that sits
    /// on the same line as a static text carrying it (a switch or popup whose label is a separate text).
    static func control(_ d: UIDriver, _ window: NSWindow, titled title: String) -> UIDriver.Element? {
        let all = d.elements(in: window).filter { $0.frame.width > 0 && $0.frame.height > 0 }
        let controls: Set<String> = ["AXCheckBox", "AXButton", "AXPopUpButton", "AXRadioButton", "AXSwitch", "AXDisclosureTriangle"]
        if let direct = all.first(where: { controls.contains($0.role) && $0.label.localizedCaseInsensitiveContains(title) }) { return direct }
        guard let text = all.first(where: { $0.role == "AXStaticText" && $0.value.localizedCaseInsensitiveContains(title) }) else { return nil }
        return all.filter { controls.contains($0.role) && abs($0.frame.midY - text.frame.midY) < 24 && $0.frame.minX >= text.frame.minX }
            .min { abs($0.frame.midY - text.frame.midY) < abs($1.frame.midY - text.frame.midY) }
    }

    static func describe(_ responder: NSResponder?) -> String {
        guard let responder else { return "nil" }
        var text = String(describing: type(of: responder))
        if let editor = responder as? NSTextView { text += " editing=\(String(describing: type(of: editor.delegate)))" }
        return text
    }

    static func focusSearch(_ d: UIDriver, _ window: NSWindow, _ field: NSSearchField) async -> Bool {
        await d.ensureKey(window)
        // Delivered to the window like a key press (an inactive app has no key window for the menu route).
        d.keyNow("f", modifiers: .command, window: window)
        let focused = await d.wait(timeout: 3) { field.currentEditor() != nil && window.firstResponder === field.currentEditor() }
        d.log("after Cmd-F: first responder \(describe(window.firstResponder)), field editor \(field.currentEditor() != nil)")
        let fields = d.views(in: window, classNameContains: "SearchField").compactMap { $0 as? NSSearchField }
        d.log("search fields in the window: \(fields.count), item's field in window: \(field.window === window), same: \(fields.contains { $0 === field }), "
            + "values \(fields.map(\.stringValue)), editors \(fields.map { $0.currentEditor() != nil })")
        if !focused {
            let direct = window.makeFirstResponder(field)
            d.log("direct makeFirstResponder returned \(direct): responder \(describe(window.firstResponder)), editor \(field.currentEditor() != nil)")
        }
        return focused
    }

    /// The profile's core preferences JSON, searched for `key` at any depth.
    static func corePreference(_ key: String) -> Any? {
        let path = LaunchOptions.current.profileDirectory + "/config/preferences.json"
        guard let data = FileManager.default.contents(atPath: path),
              let object = try? JSONSerialization.jsonObject(with: data) else { return nil }
        func find(_ value: Any) -> Any? {
            if let dictionary = value as? [String: Any] {
                if let hit = dictionary[key] { return hit }
                for child in dictionary.values { if let hit = find(child) { return hit } }
            }
            return nil
        }
        return find(object)
    }

    static func isListening(pid: Int32) -> String {
        let process = Process()
        process.executableURL = URL(filePath: "/usr/sbin/lsof")
        process.arguments = ["-nP", "-a", "-p", String(pid), "-iTCP", "-sTCP:LISTEN"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        guard (try? process.run()) != nil else { return "" }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return String(bytes: data, encoding: .utf8) ?? ""
    }

    // MARK: Search steps

    /// ⌘F, then types `query`; returns the field once the results are shown.
    static func typeSearch(_ d: UIDriver, _ window: NSWindow, _ field: NSSearchField, _ query: String) async -> Bool {
        let focused = await focusSearch(d, window, field)
        d.check(focused, "Cmd-F focused the Settings search field")
        await d.pause(0.7)  // the focus request re-evaluates the window, which rebuilds the toolbar a moment later
        d.check(window.firstResponder === field.currentEditor() && field.currentEditor() != nil, "the field kept the focus after the toolbar rebuilt")
        await d.typeFast(query, window: window, delay: 0.06)
        let typed = await d.wait(timeout: 3) { field.stringValue == query }
        d.check(typed, "search field shows '\(query)' (field value '\(field.stringValue)')")
        await d.pause(1.0)
        return typed
    }

    static func searchOnce(_ d: UIDriver, _ window: NSWindow, _ field: NSSearchField, query: String, tag: String) async throws {
        let expected = SettingsSearch.search(query)
        d.log(
            "search '\(query)': \(expected.count) expected results: "
                + expected.prefix(6).map { "\($0.title) [\($0.spec.pane.title)]" }.joined(separator: " | ")
        )
        guard let first = expected.first else {
            d.check(false, "search '\(query)' has results in this locale")
            return
        }
        // Start from another pane so the jump is observable.
        let start: SettingsPane = first.spec.pane == .speed ? .general : .speed
        _ = await openPane(d, window, start)
        await d.pause(0.8)

        // Return opens the first result.
        _ = await typeSearch(d, window, field, query)
        let shown = texts(d, window)
        d.check(shown.contains { $0.contains(first.title) }, "the results list shows '\(first.title)' (\(shown.count) texts)")
        await d.shot("settings-search-\(tag)-results")
        await d.pressReturn(window: window)
        let opened = await d.wait(timeout: 4) { window.title == first.spec.pane.title && field.stringValue.isEmpty }
        d.check(opened, "Return opened the first result's pane '\(first.spec.pane.title)' and cleared the search (title '\(window.title)')")
        await d.pause(0.55)
        d.log("pane scroll offset after the jump: \(paneScrollOffset(d, window))")
        let anchor = d.find(labelContains: first.spec.title.trimmingCharacters(in: CharacterSet(charactersIn: ": ")), in: [window]).first
        d.log("target '\(first.title)' in the pane: \(anchor?.description ?? "no row (hidden by its parent control)")")
        await d.shot("settings-search-\(tag)-return")
        await d.pause(1.6)

        // A click on a result in another pane.
        _ = await openPane(d, window, start)
        await d.pause(0.8)
        _ = await typeSearch(d, window, field, query)
        let pick = expected.dropFirst().first { $0.spec.pane != start && $0.spec.id != first.spec.id } ?? expected.dropFirst().first
        if let pick {
            let row = d.find(labelContains: pick.title, in: [window]).first { $0.role == "AXButton" }
                ?? d.find(labelContains: pick.title, in: [window]).first
            if let row {
                let done = await d.activate(row) { window.title == pick.spec.pane.title && field.stringValue.isEmpty }
                d.check(done, "click on result '\(pick.title)' opened pane '\(pick.spec.pane.title)' (title '\(window.title)', role \(row.role))")
                await d.pause(0.55)
                d.log("pane scroll offset after the click: \(paneScrollOffset(d, window))")
                await d.shot("settings-search-\(tag)-click")
                await d.pause(1.6)
            } else {
                d.check(false, "result row '\(pick.title)' not found to click")
            }
        }

        // Escape clears the query and brings the pane back.
        _ = await typeSearch(d, window, field, query)
        await d.pressEscape(window: window)
        let cleared = await d.wait(timeout: 3) { field.stringValue.isEmpty }
        d.check(cleared, "Escape cleared the search field (value '\(field.stringValue)')")
        await d.pause(0.6)
        let after = texts(d, window)
        d.check(!after.contains { $0.contains(first.title) && $0.contains("›") }, "the results list is gone after Escape")
        d.log("search field after Escape: width \(field.frame.width), editor \(field.currentEditor() != nil), hidden \(field.isHidden)")
        await d.shot("settings-search-\(tag)-escaped")
    }

    // MARK: Scenario

    /// Dumps the accessibility tree of the Advanced and General panes (to see what the driver can find).
    static func liveDump(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let window = try await openSettings(d)
        for pane in [SettingsPane.advanced, .general] {
            _ = await openPane(d, window, pane)
            await d.pause(1.5)
            d.dump(window, maxLines: 150)
        }
    }

    static func liveSettings(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let window = try await openSettings(d)
        d.check(true, "Cmd-, opened Settings: '\(window.title)' \(window.frame)")
        guard let field = settingsSearchField(window) else { throw UIDriver.Failure(description: "no search field in the Settings toolbar") }
        d.log("toolbar items: " + (window.toolbar?.items.map { $0.itemIdentifier.rawValue } ?? []).joined(separator: ", "))
        d.log("language: \(Locale.preferredLanguages.prefix(2))")
        d.log("idle search field: width \(field.frame.width), editor \(field.currentEditor() != nil), hidden \(field.isHidden)")
        await d.shot("settings-search-idle")

        // Every pane through its toolbar button.
        for pane in SettingsPane.allCases {
            let ok = await openPane(d, window, pane)
            d.check(ok && window.title == pane.title,
                    "toolbar button '\(pane.title)' shows its pane (window \(Int(window.frame.width))x\(Int(window.frame.height)))")
            await d.pause(0.5)
            await d.shot("settings-pane-\(pane.rawValue)")
        }

        // Search in each requested query (default "proxy"; Russian runs pass "proxy,прокси").
        let queries = (ProcessInfo.processInfo.environment["QBT_UI_QUERIES"] ?? "proxy").split(separator: ",").map(String.init)
        for (index, query) in queries.enumerated() {
            try await searchOnce(d, window, field, query: query, tag: "q\(index)")
        }

        // Show advanced settings: hidden rows appear.
        let startPaused = SettingsCatalog.title("BitTorrent/Session/StartPaused")
        // The switch persists in the profile, and a profile used by an earlier run starts with it on: turn it off first.
        _ = await openPane(d, window, .advanced)
        await d.pause(0.5)
        if let toggle = control(d, window, titled: SettingsCatalog.title(SettingsNavigator.showAdvancedKey)), toggle.value == "1" {
            let key = SettingsNavigator.showAdvancedKey
            d.log("'Show advanced settings' was left on by an earlier run; turning it off")
            _ = await d.activate(toggle) { !AppDefaults.store.bool(forKey: key) }
            await d.pause(0.8)
        }
        _ = await openPane(d, window, .general)
        await d.pause(0.5)
        let before = texts(d, window).contains { $0.contains(startPaused) }
        d.check(!before, "'\(startPaused)' is hidden while advanced settings are off")
        _ = await openPane(d, window, .advanced)
        await d.pause(0.5)
        let advancedTitle = SettingsCatalog.title(SettingsNavigator.showAdvancedKey)
        if let toggle = control(d, window, titled: advancedTitle) {
            let key = SettingsNavigator.showAdvancedKey
            let was = AppDefaults.store.bool(forKey: key)
            let count = d.elements(in: window).count
            let done = await d.activate(toggle) { AppDefaults.store.bool(forKey: key) != was }
            d.check(done, "'Show advanced settings' toggled (\(toggle.role)), persisted in UserDefaults: \(AppDefaults.store.bool(forKey: key))")
            await d.pause(0.8)
            d.log("Advanced pane elements: \(count) -> \(d.elements(in: window).count)")
            await d.shot("settings-advanced-on")
        } else {
            d.check(false, "'Show advanced settings' switch not found")
        }
        _ = await openPane(d, window, .general)
        await d.pause(0.8)
        d.check(texts(d, window).contains { $0.contains(startPaused) }, "'\(startPaused)' appears once advanced settings are on")
        await d.shot("settings-general-advanced")

        // A changed setting persists in the core preferences file.
        if let toggle = control(d, window, titled: startPaused) {
            let before = corePreference("start_paused") as? Bool
            let persisted = await d.activate(toggle, settle: 4) { (corePreference("start_paused") as? Bool) != before }
            d.check(persisted,
                    "'Start paused' written to preferences.json: \(String(describing: before)) -> \(String(describing: corePreference("start_paused")))")
        } else {
            d.check(false, "'\(startPaused)' toggle not found")
        }
        // A GUI preference lands in UserDefaults.
        let updateTitle = SettingsCatalog.title("Preferences/Advanced/updateCheck")
        if let toggle = control(d, window, titled: updateTitle) {
            let before = d.model.gui.checkForUpdates
            let done = await d.activate(toggle) { d.model.gui.checkForUpdates != before }
            let stored = AppDefaults.store.object(forKey: GUIPreferenceKey.checkForUpdates.rawValue) as? Bool
            d.check(done && stored == d.model.gui.checkForUpdates,
                    "'\(updateTitle)' changed to \(d.model.gui.checkForUpdates) and stored in UserDefaults (\(String(describing: stored)))")
        } else {
            d.check(false, "'\(updateTitle)' toggle not found")
        }

        await liveSettingsFixes(d, window)

        // Test Port: only when the app is listening, once.
        _ = await openPane(d, window, .network)
        await d.pause(0.8)
        let pid = ProcessInfo.processInfo.processIdentifier
        let listening = isListening(pid: pid)
        d.log("listening sockets of pid \(pid): \(listening.isEmpty ? "none" : listening)")
        let testTitle = SettingsCatalog.title("system.testPort")
        if let button = control(d, window, titled: testTitle) {
            if listening.isEmpty || ProcessInfo.processInfo.environment["QBT_UI_TESTPORT"] != "1" {
                d.log("SKIP: Test Port not pressed (listening: \(!listening.isEmpty), QBT_UI_TESTPORT=1 needed; button enabled=\(button.enabled))")
            } else {
                d.log("pressing Test Port once (the app is listening)")
                await d.click(button)
                await d.pause(8)
                await d.shot("settings-test-port")
                d.log("window texts after Test Port: \(texts(d, window).filter { $0.localizedCaseInsensitiveContains("port") }.prefix(8))")
            }
        } else {
            d.log("Test Port button not found")
        }
        window.close()
        await d.pause(0.5)
        d.check(settingsWindow(d) == nil, "Settings window closed")
    }
}
#endif
