import AppKit

/// The app, File, Window and Help menus: the standard items SwiftUI used to
/// add, plus the commands placed among them.
extension MainMenuController {
    func makeAppMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.appName)
        let about = MenuItemAction { [model] in model.show(.about) }
        menu.addItem(about.makeItem(String(localized: "About qBit NeoRS")))
        menu.addItem(.separator())
        let settings = MenuItemAction { [model] in
            if model.openSettings != nil {
                model.showSettings()
            } else {
                NSApp.sendAction(Selector(("showSettingsWindow:")), to: nil, from: nil)
            }
        }
        menu.addItem(settings.makeItem(SystemMenuTitle.text("Settings…"), symbol: Symbol.gear, shortcut: CommandShortcut(",")))
        menu.addItem(.separator())
        let services = NSMenuItem(title: SystemMenuTitle.text("Services"), action: nil, keyEquivalent: "")
        servicesMenu = NSMenu(title: services.title)
        services.submenu = servicesMenu
        menu.addItem(services)
        menu.addItem(.separator())
        menu.addItem(standardItem(SystemMenuTitle.withAppName("Hide %@"), #selector(NSApplication.hide(_:)), "h"))
        menu.addItem(standardItem(SystemMenuTitle.text("Hide Others"), #selector(NSApplication.hideOtherApplications(_:)), "h", [.command, .option]))
        menu.addItem(standardItem(SystemMenuTitle.text("Show All"), #selector(NSApplication.unhideAllApplications(_:))))
        menu.addItem(.separator())
        // Not `terminate:` directly: open sheets must close first (`TerminationController`).
        let quit = MenuItemAction { [model] in model.termination.requestQuit() }
        menu.addItem(quit.makeItem(SystemMenuTitle.withAppName("Quit %@"), shortcut: CommandShortcut("q")))
        return menu
    }

    func makeFileMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.text("File"))
        menu.addItem(commandItem(.addTorrentFile))
        menu.addItem(commandItem(.addTorrentLink))
        menu.addItem(.separator())
        menu.addItem(commandItem(.importFromQBittorrent))
        menu.addItem(commandItem(.importFromTransmission))
        menu.addItem(.separator())
        menu.addItem(standardItem(SystemMenuTitle.text("Close"), #selector(NSWindow.performClose(_:)), "w"))
        let closeAll = standardItem(SystemMenuTitle.text("Close All"), Selector(("closeAll:")), "w", [.command, .option])
        closeAll.isAlternate = true
        menu.addItem(closeAll)
        return menu
    }

    func makeWindowMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.text("Window"))
        menu.addItem(standardItem(SystemMenuTitle.text("Minimize"), #selector(NSWindow.performMiniaturize(_:)), "m"))
        menu.addItem(standardItem(SystemMenuTitle.text("Zoom"), #selector(NSWindow.performZoom(_:))))
        menu.addItem(.separator())
        // One item per single-window scene, as SwiftUI lists them.
        let scenes: [(String, String)] = [
            (AppWindowID.main.title, AppWindowID.main.rawValue),
            (AppWindowID.creator.title, AppWindowID.creator.rawValue),
            (AppWindowID.statistics.title, AppWindowID.statistics.rawValue),
            (AppWindowID.cookies.title, AppWindowID.cookies.rawValue),
            (FeatureWindowID.rssDownloader.title, FeatureWindowID.rssDownloader.rawValue),
            (FeatureWindowID.searchPlugins.title, FeatureWindowID.searchPlugins.rawValue),
            (AppWindowID.about.title, AppWindowID.about.rawValue),
        ]
        for (title, id) in scenes {
            let open = MenuItemAction { [model] in model.openWindow?(id: id) }
            menu.addItem(open.makeItem(title))
        }
        menu.addItem(.separator())
        menu.addItem(standardItem(SystemMenuTitle.text("Bring All to Front"), #selector(NSApplication.arrangeInFront(_:))))
        windowMenu = menu
        return menu
    }

    func makeHelpMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.text("Help"))
        menu.addItem(commandItem(.documentation, lockGated: false))
        menu.addItem(.separator())
        let donate = MenuItemAction {
            if let url = URL(string: "https://www.qbittorrent.org/donate") { NSWorkspace.shared.open(url) }
        }
        menu.addItem(donate.makeItem(String(localized: "Donate!")))
        helpMenu = menu
        return menu
    }

    /// An item sending a standard action along the responder chain.
    func standardItem(_ title: String, _ action: Selector, _ key: String = "", _ modifiers: NSEvent.ModifierFlags = .command) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        item.keyEquivalentModifierMask = key.isEmpty ? [] : modifiers
        return item
    }
}
