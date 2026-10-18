#if DEBUG
import AppKit

/// Scenario "quit-presented": quitting while something is presented must end the process (after the core
/// shutdown). `QBT_UI_ARG` picks what is open: `none`, `sheet`, `remove` (adds a torrent, then the remove
/// sheet), `alert`, `confirm`, `nested` (an AppKit alert sheet on the SwiftUI sheet), `appkit-sheet`, `add`
/// (Add Torrent window), `settings-sheet` (an alert sheet on Settings), `nsalert` (app-modal alert), `menu`
/// (a menu tracking), `closed` (main window closed and released). `QBT_UI_QUIT` picks how the quit arrives:
/// `external` (another process sends it after `ready` appears in `QBT_UI_DIR`), `cmdq` (posted Cmd-Q),
/// `menubar` (the menu bar extra's Quit action), `task` (a request from inside a main-actor task, like the
/// core's shutdown request) or `terminate` (`NSApp.terminate` from a run-loop timer).
/// `QBT_UI_CONFIRM=no-then-yes` turns the exit confirmation on (use `--demo`, whose torrents transfer) and
/// answers No to the first quit (the sheet must stay) and Yes to the second.
extension UIScenarios {
    static func quitPresented(_ d: UIDriver) async throws {
        let environment = ProcessInfo.processInfo.environment
        let what = environment["QBT_UI_ARG"] ?? "sheet"
        let how = environment["QBT_UI_QUIT"] ?? "external"
        let confirm = environment["QBT_UI_CONFIRM"] ?? ""
        try await waitRunning(d)
        if !confirm.isEmpty {
            d.model.gui.confirmOnExit = true
            _ = await d.wait(timeout: 20) { d.model.status.isTransferring }
            d.check(d.model.status.isTransferring, "torrents are transferring (exit confirmation applies)")
        }
        try await present(what, d)
        await d.pause(1.5)
        d.log("presented \(what): sheets=\(sheetSummary()) modal=\(NSApp.modalWindow?.title ?? "-")")
        if confirm == "no-then-yes" {
            answerExitConfirmation(d, "No")
            try await sendQuit(how, d)
            await d.pause(5)
            d.check(NSApp.modalWindow == nil, "no modal window left after answering No")
            d.check(what == "none" || !sheetSummary().isEmpty, "after No the sheet is still open: \(sheetSummary())")
            d.check(!d.model.termination.isShuttingDown, "after No the app keeps running")
            d.log("--- second quit, answering Yes")
            answerExitConfirmation(d, "Yes")
        }
        try await sendQuit(how, d)
        for second in stride(from: 2, through: 40, by: 2) {
            await d.pause(2)
            d.log("t+\(second)s: sheets=\(sheetSummary()) modal=\(NSApp.modalWindow?.title ?? "-") shuttingDown=\(d.model.termination.isShuttingDown)")
        }
        d.log("FAIL: process still alive 40 s after the quit request (\(what), \(how))")
    }

    private static var readyPath: String {
        ProcessInfo.processInfo.environment["QBT_UI_DIR", default: NSTemporaryDirectory()] + "/ready"
    }

    private static func sheetSummary() -> [String] {
        NSApp.windows.compactMap(\.attachedSheet).map { "\(type(of: $0))" }
    }

    private static func sendQuit(_ how: String, _ d: UIDriver) async throws {
        switch how {
        case "cmdq":
            await d.command("q")
        case "menubar":
            // What MenuBarContent's Quit button runs.
            d.model.termination.requestQuit()
        case "task":
            Task { @MainActor in d.model.termination.requestQuit() }
        case "terminate":
            d.afterCommon(0.1) { NSApp.terminate(nil) }
        default:
            FileManager.default.createFile(atPath: readyPath, contents: nil)
            d.log("READY for the external quit event")
        }
    }

    /// Clicks `button` in the exit confirmation once it is up (it runs a modal loop, so this is a run-loop timer).
    private static func answerExitConfirmation(_ d: UIDriver, _ button: String) {
        let deadline = Date.now.addingTimeInterval(30)
        let done = Flag()
        let timer = Timer(timeInterval: 0.3, repeats: true) { _ in
            MainActor.assumeIsolated {
                guard !done.value else { return }
                if Date.now > deadline {
                    d.log("FAIL: no exit confirmation appeared")
                    done.value = true
                    return
                }
                guard let alert = NSApp.modalWindow,
                      let target = d.find(role: "AXButton", label: button, in: [alert]).first else { return }
                done.value = true
                let texts = d.elements(in: alert).filter { $0.role == "AXStaticText" }.map(\.value)
                d.log("PASS: exit confirmation shown \(texts); sheets=\(sheetSummary()); clicking \(button)")
                d.clickNow(target)
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        RunLoop.main.add(timer, forMode: .modalPanel)
    }

    private static func present(_ what: String, _ d: UIDriver) async throws {
        switch what {
        case "sheet":
            try await presentSheet(.speedLimits, d)
        case "remove":
            try await addTorrentsThroughDialogs(d, paths: Array(torrentPaths.prefix(1)))
            guard let item = d.model.transfers.items.first else { throw UIDriver.Failure(description: "no torrents") }
            try await presentSheet(.removeTorrents(ids: [item.id], deleteFiles: false), d)
        case "alert":
            d.model.alert = AppAlert(title: "Quit test", message: "An alert is open.")
            await waitForSheet(d, "alert")
        case "confirm":
            guard let item = d.model.transfers.items.first else { throw UIDriver.Failure(description: "no torrents (use --demo)") }
            d.model.confirmation = .mergeTrackers(hash: item.id, name: item.name, trackers: [], urlSeeds: [])
            await waitForSheet(d, "confirmation dialog")
        case "nested":
            try await presentSheet(.speedLimits, d)
            guard let sheet = try mainWindow(d).attachedSheet else { throw UIDriver.Failure(description: "no sheet") }
            beginAlertSheet(on: sheet, d)
        case "appkit-sheet":
            beginAlertSheet(on: try mainWindow(d), d)
        case "settings-sheet":
            beginAlertSheet(on: try await openSettings(d), d)
        case "add":
            guard let path = torrentPaths.first else { throw UIDriver.Failure(description: "QBT_UI_TORRENT is empty") }
            d.model.open([URL(filePath: path)])
            _ = try await d.require(role: "AXButton", label: "Add", timeout: 15)
            d.check(true, "Add Torrent dialog is open")
        case "nsalert":
            d.afterCommon(0.1) {
                let alert = NSAlert()
                alert.messageText = "Quit test (app-modal)"
                let response = alert.runModal()
                d.log("app-modal alert ended with \(response.rawValue)")
            }
            _ = await d.wait(timeout: 5) { NSApp.modalWindow != nil }
            d.check(NSApp.modalWindow != nil, "app-modal alert is running")
        case "menu":
            d.afterCommon(0.1) {
                let menu = NSMenu(title: "Quit test")
                menu.addItem(withTitle: "Item", action: nil, keyEquivalent: "")
                d.afterCommon(15) { menu.cancelTracking() }
                d.log("menu tracking starts")
                menu.popUp(positioning: nil, at: .zero, in: d.model.mainWindow?.contentView)
                d.log("menu tracking ended")
            }
        case "closed":
            try mainWindow(d).performClose(nil)
            await d.pause(6)
            d.check(!d.model.isMainWindowShown, "main window closed (released after QBT_WINDOW_RELEASE_DELAYS)")
        default:
            break
        }
    }

    private static func presentSheet(_ sheet: MainSheet, _ d: UIDriver) async throws {
        d.model.sheet = sheet
        await waitForSheet(d, "\(sheet.id) sheet")
    }

    private static func waitForSheet(_ d: UIDriver, _ what: String) async {
        _ = await d.wait(timeout: 10) { !sheetSummary().isEmpty }
        d.check(!sheetSummary().isEmpty, "\(what) is open: \(sheetSummary())")
    }

    private static func beginAlertSheet(on window: NSWindow, _ d: UIDriver) {
        let alert = NSAlert()
        alert.messageText = "Quit test (sheet)"
        alert.beginSheetModal(for: window) { response in d.log("alert sheet ended with \(response.rawValue)") }
        d.check(window.attachedSheet != nil, "AppKit alert sheet is attached to '\(window.title)'")
    }
}
#endif
