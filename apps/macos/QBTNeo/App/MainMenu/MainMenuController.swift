import AppKit
import Observation

/// The main menu, built once with AppKit from the command registry instead of
/// SwiftUI `Commands`. SwiftUI keeps a view graph per menu item alive for the
/// whole run (several MB for this menu); `NSMenuItem`s validated on demand
/// cost a few KB and do no work until a menu opens or a shortcut is pressed.
///
/// Items, titles, symbols, shortcuts, order and enabled/on states match the
/// former SwiftUI menu; standard titles come from SwiftUI's own string table
/// (`SystemMenuTitle`). SwiftUI still builds its default menu into the main
/// menu when its scene graph changes, so the controller puts its own items
/// back right after (`MainMenuRoot`).
@MainActor
final class MainMenuController {
    let model: AppModel
    let menu = MainMenuRoot()
    /// Menus AppKit fills itself (window list, Help search, Services).
    var windowMenu = NSMenu()
    var helpMenu = NSMenu()
    var servicesMenu = NSMenu()
    /// Edit-menu items shown only while queueing is on, and the two session items.
    var queueItems: [NSMenuItem] = []
    var pauseSessionItem: NSMenuItem?
    var resumeSessionItem: NSMenuItem?
    private var topLevelItems: [NSMenuItem] = []
    private var isRestoreScheduled = false
    private let symbolLoader = MenuSymbolLoader()

    init(model: AppModel) {
        self.model = model
    }

    func install() {
        topLevelItems = [
            submenuItem(makeAppMenu()),
            submenuItem(makeFileMenu()),
            submenuItem(makeEditMenu()),
            submenuItem(makeViewMenu()),
            submenuItem(makeToolsMenu()),
            submenuItem(makeWindowMenu()),
            submenuItem(makeHelpMenu()),
        ]
        symbolLoader.adopt(topLevelItems, except: servicesMenu)
        updateStructure()
        observeStructure()
        menu.onForeignEdit = { [weak self] in self?.scheduleRestore() }
        restore()
        // Should SwiftUI ever install a menu of its own instead, take over
        // again before the menu bar is used.
        NotificationCenter.default.addObserver(forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.restore() }
        }
    }

    /// Puts the menu bar back to this menu: its own top-level items, and the
    /// Window, Help and Services menus AppKit maintains.
    private func restore() {
        isRestoreScheduled = false
        if menu.items != topLevelItems {
            menu.isEditing = true
            menu.items = topLevelItems
            menu.isEditing = false
        }
        if NSApp.mainMenu !== menu { NSApp.mainMenu = menu }
        if NSApp.servicesMenu !== servicesMenu { NSApp.servicesMenu = servicesMenu }
        if NSApp.windowsMenu !== windowMenu { NSApp.windowsMenu = windowMenu }
        if NSApp.helpMenu !== helpMenu { NSApp.helpMenu = helpMenu }
    }

    /// SwiftUI edits the menu in several steps; restore once it is done.
    private func scheduleRestore() {
        guard !isRestoreScheduled else { return }
        isRestoreScheduled = true
        DispatchQueue.main.async { [weak self] in self?.restore() }
    }

    private func submenuItem(_ submenu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem(title: submenu.title, action: nil, keyEquivalent: "")
        item.submenu = submenu
        return item
    }

    // MARK: Items that come and go

    /// Queue items follow the queueing setting and Pause/Resume Session the
    /// session state, like the conditional items of the former SwiftUI menu.
    /// Hidden items never take a key equivalent, so this runs on change rather
    /// than when the menu opens.
    private func updateStructure() {
        let queueing = model.queueingEnabled
        for item in queueItems where item.isHidden == queueing { item.isHidden = !queueing }
        let paused = model.status.isPaused
        if pauseSessionItem?.isHidden != paused { pauseSessionItem?.isHidden = paused }
        if resumeSessionItem?.isHidden != !paused { resumeSessionItem?.isHidden = !paused }
    }

    private func observeStructure() {
        withObservationTracking {
            _ = model.queueingEnabled
            _ = model.status.isPaused
        } onChange: { [weak self] in
            Task { @MainActor in
                self?.updateStructure()
                self?.observeStructure()
            }
        }
    }
}
