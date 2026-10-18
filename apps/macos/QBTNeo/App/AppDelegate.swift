import AppKit
import SwiftUI

/// AppKit integration SwiftUI has no equivalent for: termination (run by
/// `TerminationController`), the Dock menu, Finder "Open With", the Dock
/// badge, and snapshot mode.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model: AppModel
    private var dockBadge: DockBadgeController?
    private var systemIntegration: SystemIntegration?
    private var mainMenu: MainMenuController?
    #if DEBUG
    private var floatTimer: Timer?
    #endif

    override init() {
        // Before any window: keeps AppKit's autosaving out of the real domain.
        StandardDefaultsRedirect.install(redirectingTo: AppDefaults.store)
        if LaunchOptions.current.profileDirectory.isEmpty, !LaunchOptions.current.usesDemoData {
            LegacyDefaultsMigration.runIfNeeded()
        }
        // Before `GUIPreferences` loads: a pending import's GUI settings apply on this launch.
        PendingImportSettings.applyIfNeeded(LaunchOptions.current)
        model = AppModel()
        // Never restore or save AppKit window state. The main window is always
        // presented at launch; a run that ended with no window open (snapshot
        // mode, quitting after closing it) leaves a "zero windows" state behind
        // that AppKit would restore on every later launch, and SwiftUI then
        // never presents the main window. State is keyed by the code-signing
        // identifier, so it also survives rebuilds.
        UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true])
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let directory = model.launch.snapshotDirectory {
            SettingsSnapshot.appModel = model
            Task {
                await SnapshotRenderer(directory: directory, launch: model.launch).run()
                NSApp.terminate(nil)
            }
            return
        }
        model.gui.applyAppearance()
        model.termination.installQuitEventHandling()
        let mainMenu = MainMenuController(model: model)
        mainMenu.install()
        self.mainMenu = mainMenu
        MemoryPressureMonitor.shared.start()
        #if DEBUG
        floatTimer = DebugVerification.install(model: model)
        #endif
        dockBadge = DockBadgeController(model: model)
        systemIntegration = SystemIntegration(app: model)
        if model.launch.publishesWidgets {
            let widgets = WidgetPublisher(app: model)
            model.widgets = widgets
            widgets.start()
        }
        Task { await model.start() }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        model.termination.shouldTerminate()
    }

    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        DockMenuBuilder(model: model).makeMenu()
    }
}
