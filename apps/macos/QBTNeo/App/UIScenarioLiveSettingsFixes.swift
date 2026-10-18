#if DEBUG
import AppKit

/// Live-run checks of the Settings fixes, run by "live-settings" after the pane and search checks: the status
/// bar's External IP follows Settings, a path gated by an unchecked option is not validated, and removing a
/// watched folder asks first (Cancel and Escape keep it, Remove removes it).
extension UIScenarios {
    private static func hasCyrillic(_ text: String) -> Bool {
        text.unicodeScalars.contains { (0x0400...0x04FF).contains($0.value) }
    }

    private static func statusBarShowsExternalIP(_ d: UIDriver) -> Bool {
        guard let main = d.model.mainWindow else { return false }
        // The item's text starts with the localized "External IP:" or "External IPs:".
        let single = String(localized: "External IP: N/A").prefix { $0 != ":" }
        let ipv4 = "203.0.113.1", ipv6 = "2001:db8::1"
        let plural = String(localized: "External IPs: \(ipv4), \(ipv6)").prefix { $0 != ":" }
        return d.elements(in: main).contains { element in
            [element.value, element.label].contains { $0.hasPrefix(single) || $0.hasPrefix(plural) }
        }
    }

    private static func checkboxOn(_ element: UIDriver.Element?) -> Bool? {
        guard let element else { return nil }
        return element.value == "1"
    }

    static func liveSettingsFixes(_ d: UIDriver, _ window: NSWindow) async {
        await externalIPFollowsSettings(d, window)
        await disabledPathIsNotValidated(d, window)
        await watchedFolderRemovalAsksFirst(d, window)
    }

    private static func externalIPFollowsSettings(_ d: UIDriver, _ window: NSWindow) async {
        d.model.showsExternalIP = false
        _ = await openPane(d, window, .interface)
        await d.pause(1)
        let title = SettingsCatalog.title("status_bar_external_ip")
        guard let toggle = control(d, window, titled: title) else {
            d.check(false, "'\(title)' toggle not found in Interface")
            return
        }
        d.check(checkboxOn(toggle) == false, "'\(title)' is off in Settings after the app turned it off (value '\(toggle.value)')")
        d.check(!statusBarShowsExternalIP(d), "status bar hides External IP while the option is off")

        _ = await d.activate(toggle, settle: 4) { d.model.corePreferences.behavior.statusBarExternalIp }
        let shown = await d.wait(timeout: 6) { statusBarShowsExternalIP(d) }
        d.check(d.model.showsExternalIP && shown, "Settings: turning on '\(title)' shows External IP in the status bar at once "
            + "(model \(d.model.showsExternalIP), the status bar's menu toggle reads the same property)")

        _ = await d.activate(toggle, settle: 4) { !d.model.corePreferences.behavior.statusBarExternalIp }
        let hidden = await d.wait(timeout: 6) { !statusBarShowsExternalIP(d) }
        d.check(!d.model.showsExternalIP && hidden, "Settings: turning it off hides External IP in the status bar at once")

        // The other direction: the status bar's own toggle (the context menu writes this property) updates Settings.
        d.model.showsExternalIP = true
        let follows = await d.wait(timeout: 6) { checkboxOn(control(d, window, titled: title)) == true }
        d.check(follows && statusBarShowsExternalIP(d), "turning it on from the app updates the Settings checkbox and the status bar")
        d.model.showsExternalIP = false
        let back = await d.wait(timeout: 6) { checkboxOn(control(d, window, titled: title)) == false }
        d.check(back && !statusBarShowsExternalIP(d), "turning it off from the app updates the Settings checkbox and the status bar")
    }

    private static func disabledPathIsNotValidated(_ d: UIDriver, _ window: NSWindow) async {
        var prefs = d.model.corePreferences
        prefs.downloads.tempPath = "/nonexistent-qbt-scratch/incomplete"
        prefs.downloads.tempPathEnabled = false
        d.model.savePreferences(prefs)
        _ = await openPane(d, window, .transfers)
        await d.pause(1.5)
        let title = SettingsCatalog.title("temp_path_enabled")
        let warning = String(localized: "Path does not exist")
        func warned() -> Bool { texts(d, window).contains { $0.contains(warning) } }
        guard let toggle = control(d, window, titled: title) else {
            d.check(false, "'\(title)' checkbox not found in Transfers")
            return
        }
        d.check(checkboxOn(toggle) == false && !warned(), "unchecked '\(title)' with a missing path shows no '\(warning)'")
        _ = await d.activate(toggle, settle: 3) { d.model.corePreferences.downloads.tempPathEnabled || checkboxOn(control(d, window, titled: title)) == true }
        d.check(await d.wait(timeout: 5) { warned() }, "checked '\(title)' shows '\(warning)'")
        if let again = control(d, window, titled: title) {
            _ = await d.activate(again, settle: 3) { checkboxOn(control(d, window, titled: title)) == false }
        }
        d.check(await d.wait(timeout: 5) { !warned() }, "unchecking it again removes the warning")
    }

    private static func alertSheet(_ d: UIDriver, _ window: NSWindow) -> NSWindow? {
        window.attachedSheet ?? d.windows.first { $0.isSheet }
    }

    private static func watchedFolderRemovalAsksFirst(_ d: UIDriver, _ window: NSWindow) async {
        await d.pause(2.5) // lets Settings finish applying its last edit, or it would overwrite this change
        let folder = LaunchOptions.current.profileDirectory + "/watched-scratch"
        try? FileManager.default.createDirectory(atPath: folder, withIntermediateDirectories: true)
        var prefs = d.model.corePreferences
        prefs.downloads.watchedFolders = [WatchedFolder(path: folder, recursive: false, params: .defaults)]
        prefs.downloads.tempPathEnabled = false
        d.model.savePreferences(prefs)
        _ = await openPane(d, window, .transfers)
        await d.pause(1.5)
        func listed() -> Bool { texts(d, window).contains { $0.contains(folder) } }
        func removeButton() -> UIDriver.Element? {
            d.elements(in: window).first { $0.role == "AXButton" && $0.label == String(localized: "Remove") && $0.frame.width > 0 }
        }
        d.check(listed(), "the watched folder is listed in Transfers")

        // Cancel button.
        guard let remove = removeButton() else { d.check(false, "row Remove button not found"); return }
        _ = await d.activate(remove, settle: 3) { alertSheet(d, window) != nil }
        guard let alert = alertSheet(d, window) else { d.check(false, "Remove did not show a confirmation"); return }
        let alertTexts = d.elements(in: alert).flatMap { [$0.value, $0.label] }.filter { !$0.isEmpty }
        d.log("removal alert texts: \(alertTexts)")
        d.check(alertTexts.contains { $0.contains("watched-scratch") }, "the confirmation names the folder")
        let russian = Locale.preferredLanguages.first?.hasPrefix("ru") == true
        if russian {
            d.check(alertTexts.contains(where: hasCyrillic) && !alertTexts.contains { $0.contains("Torrents already added") },
                    "Russian locale: the confirmation is translated")
        }
        if let cancel = d.elements(in: alert).first(where: { $0.role == "AXButton" && $0.label == String(localized: "Cancel") }) {
            _ = await d.activate(cancel, settle: 3) { alertSheet(d, window) == nil }
        }
        d.check(alertSheet(d, window) == nil && listed(), "Cancel closes the confirmation and keeps the folder")

        // Escape.
        if let remove = removeButton() {
            _ = await d.activate(remove, settle: 3) { alertSheet(d, window) != nil }
            if let alert = alertSheet(d, window) {
                await d.pressEscape(window: alert)
                _ = await d.wait(timeout: 3) { alertSheet(d, window) == nil }
            }
        }
        d.check(alertSheet(d, window) == nil && listed(), "Escape closes the confirmation and keeps the folder")

        // Remove.
        if let remove = removeButton() {
            _ = await d.activate(remove, settle: 3) { alertSheet(d, window) != nil }
            if let alert = alertSheet(d, window),
               let confirm = d.elements(in: alert).first(where: { $0.role == "AXButton" && $0.label == String(localized: "Remove") }) {
                _ = await d.activate(confirm, settle: 3) { alertSheet(d, window) == nil }
            }
        }
        let gone = await d.wait(timeout: 6) { !listed() && d.model.corePreferences.downloads.watchedFolders.isEmpty }
        d.check(gone, "Remove in the confirmation removes the folder (applied: \(d.model.corePreferences.downloads.watchedFolders.count) left)")
    }
}
#endif
