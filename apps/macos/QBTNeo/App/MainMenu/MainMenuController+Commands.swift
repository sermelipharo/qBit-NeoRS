import AppKit

/// The Edit, View and Tools menus, rendered from the command registry.
extension MainMenuController {
    func makeEditMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.text("Edit"))
        menu.addItem(standardItem(SystemMenuTitle.text("Undo"), Selector(("undo:")), "z"))
        menu.addItem(standardItem(SystemMenuTitle.text("Redo"), Selector(("redo:")), "z", [.command, .shift]))
        menu.addItem(.separator())
        menu.addItem(standardItem(SystemMenuTitle.text("Cut"), #selector(NSText.cut(_:)), "x"))
        menu.addItem(standardItem(SystemMenuTitle.text("Copy"), #selector(NSText.copy(_:)), "c"))
        menu.addItem(standardItem(SystemMenuTitle.text("Paste"), #selector(NSText.paste(_:)), "v"))
        menu.addItem(standardItem(SystemMenuTitle.text("Delete"), #selector(NSText.delete(_:))))
        menu.addItem(standardItem(SystemMenuTitle.text("Select All"), #selector(NSText.selectAll(_:)), "a"))
        menu.addItem(.separator())
        // ⌘F: the main window's toolbar search field (filter, plugin search, RSS or log filter by tab).
        let find = MenuItemAction(
            isEnabled: { [model] in !model.lock.isLocked && model.mainWindow?.isKeyWindow == true },
            perform: { [model] in model.focusSearchField() }
        )
        let findItem = find.makeItem(SystemMenuTitle.text("Find…"))
        findItem.keyEquivalent = "f"
        findItem.keyEquivalentModifierMask = [.command]
        menu.addItem(findItem)
        menu.addItem(.separator())
        for id in [CommandID.start, .stop, .forceStart, .remove, .forceRecheck] {
            menu.addItem(commandItem(id))
        }
        let queueSeparator = NSMenuItem.separator()
        menu.addItem(queueSeparator)
        queueItems = [queueSeparator]
        for id in [CommandID.queueTop, .queueUp, .queueDown, .queueBottom] {
            let item = commandItem(id)
            queueItems.append(item)
            menu.addItem(item)
        }
        menu.addItem(.separator())
        menu.addItem(commandItem(.startAll))
        menu.addItem(commandItem(.stopAll))
        menu.addItem(.separator())
        let pause = commandItem(.pauseSession)
        let resume = commandItem(.resumeSession)
        pauseSessionItem = pause
        resumeSessionItem = resume
        menu.addItem(pause)
        menu.addItem(resume)
        return menu
    }

    func makeViewMenu() -> NSMenu {
        let menu = NSMenu(title: SystemMenuTitle.text("View"))
        // Like Finder's "as Icons ⌘1 / as List ⌘2": the transfer list as a table or as cards.
        let viewModeGUI = model.gui
        for mode in TransferViewMode.allCases {
            let id: CommandID = mode == .list ? .viewAsList : .viewAsCards
            let action = MenuItemAction(
                isEnabled: unlocked, isOn: { viewModeGUI.transferViewMode == mode }, perform: { viewModeGUI.transferViewMode = mode }
            )
            menu.addItem(action.makeItem(mode.title, shortcut: CommandRegistry[id].shortcut))
        }
        menu.addItem(.separator())
        menu.addItem(sortMenuItem())
        menu.addItem(.separator())
        menu.addItem(standardItem(SystemMenuTitle.text("Show Toolbar"), #selector(NSWindow.toggleToolbarShown(_:)), "t", [.command, .option]))
        menu.addItem(standardItem(SystemMenuTitle.text("Customize Toolbar…"), #selector(NSWindow.runToolbarCustomizationPalette(_:))))
        menu.addItem(.separator())
        let palette = MenuItemAction(isEnabled: unlocked) { [model] in model.palette.toggle() }
        menu.addItem(palette.makeItem(
            String(localized: "palette.menu", defaultValue: "Command Palette…", comment: "View menu item opening the Command Palette"),
            symbol: Symbol.command, shortcut: CommandShortcut("k")
        ))
        menu.addItem(.separator())
        let gui = model.gui
        menu.addItem(toggleItem(String(localized: "Status Bar"), isOn: { gui.statusBarDisplayed }, toggle: { gui.statusBarDisplayed.toggle() }))
        menu.addItem(toggleItem(String(localized: "Filters Sidebar"), isOn: { gui.filtersSidebarVisible }, toggle: { gui.filtersSidebarVisible.toggle() }))
        menu.addItem(toggleItem(String(localized: "Speed in Title Bar"), isOn: { gui.speedInTitleBar }, toggle: { gui.speedInTitleBar.toggle() }))
        menu.addItem(sidebarSectionsItem())
        menu.addItem(.separator())
        menu.addItem(commandItem(.searchEngine))
        menu.addItem(commandItem(.rssReader))
        menu.addItem(logMenuItem())
        menu.addItem(.separator())
        menu.addItem(commandItem(.lock))
        menu.addItem(commandItem(.setLockPassword))
        let clear = MenuItemAction(
            isEnabled: { [model] in !model.lock.isLocked && model.lock.hasPassword },
            perform: { [model] in model.confirmation = .clearLockPassword }
        )
        menu.addItem(clear.makeItem(String(localized: "Clear Password…")))
        menu.addItem(.separator())
        menu.addItem(commandItem(.statistics))
        menu.addItem(.separator())
        menu.addItem(commandItem(.globalSpeedLimits))
        let model = model
        let altSpeed = toggleItem(
            String(localized: "Alternative Speed Limits"), isOn: { model.alternativeSpeedLimits }, toggle: { model.alternativeSpeedLimits.toggle() }
        )
        if let shortcut = CommandRegistry[.alternativeSpeedLimits].shortcut {
            altSpeed.keyEquivalent = shortcut.menuKeyEquivalent
            altSpeed.keyEquivalentModifierMask = shortcut.menuModifierMask
        }
        menu.addItem(altSpeed)
        menu.addItem(.separator())
        let sidebar = standardItem(SystemMenuTitle.text("Show Sidebar"), #selector(NSSplitViewController.toggleSidebar(_:)), "s", [.command, .control])
        menu.addItem(sidebar)
        return menu
    }

    func makeToolsMenu() -> NSMenu {
        let menu = NSMenu(title: String(localized: "Tools"))
        menu.addItem(commandItem(.torrentCreator))
        menu.addItem(.separator())
        menu.addItem(commandItem(.manageCookies))
        menu.addItem(commandItem(.options))
        menu.addItem(.separator())
        let completion = NSMenuItem(title: String(localized: "On Downloads Done"), action: nil, keyEquivalent: "")
        completion.keyEquivalentModifierMask = []
        let choices = NSMenu(title: completion.title)
        let gui = model.gui
        for action in CompletionAction.allCases {
            choices.addItem(toggleItem(action.title, isOn: { gui.completionAction == action }, toggle: { gui.completionAction = action }))
        }
        completion.submenu = choices
        menu.addItem(completion)
        return menu
    }

    private func logMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: String(localized: "Log"), action: nil, keyEquivalent: "")
        item.keyEquivalentModifierMask = []
        let menu = NSMenu(title: item.title)
        menu.addItem(commandItem(.executionLog))
        menu.addItem(.separator())
        let gui = model.gui
        let types: [(String, Int)] = [
            (String(localized: "Normal Messages"), 0x1), (String(localized: "Information Messages"), 0x2),
            (String(localized: "Warning Messages"), 0x4), (String(localized: "Critical Messages"), 0x8),
        ]
        for (title, flag) in types {
            menu.addItem(toggleItem(title, isOn: { gui[logType: flag] }, toggle: { gui[logType: flag].toggle() }))
        }
        item.submenu = menu
        return item
    }

    // MARK: Item factories

    /// A registry command acting on the transfer-list selection at the time
    /// it is validated or chosen; disabled while the UI is locked unless
    /// `lockGated` is false.
    func commandItem(_ id: CommandID, lockGated: Bool = true) -> NSMenuItem {
        let command = CommandRegistry[id]
        let model = model
        let action = MenuItemAction(
            isEnabled: { !(lockGated && model.lock.isLocked) && command.canRun(in: CommandContext(selectionOf: model)) },
            perform: {
                let context = CommandContext(selectionOf: model)
                guard command.canRun(in: context) else { return }
                command.perform(context)
            }
        )
        return action.makeItem(command.title, symbol: command.symbol, shortcut: command.shortcut)
    }

    /// An on/off item (a SwiftUI `Toggle` in a menu): a check mark, no symbol.
    /// View › Sidebar Sections: brings back sections hidden from the sidebar's context menu.
    private func sidebarSectionsItem() -> NSMenuItem {
        let item = NSMenuItem(title: String(localized: "Sidebar Sections"), action: nil, keyEquivalent: "")
        let submenu = NSMenu(title: item.title)
        let model = model
        for section in SidebarItem.Section.allCases {
            if section == .trackerStatus {
                // Only exists as a section when Tracker Status is shown separately.
                let gui = model.gui
                let action = MenuItemAction(
                    isEnabled: { [model] in !model.lock.isLocked && gui.separateTrackerStatusFilter },
                    isOn: { !gui.hiddenSidebarSections.contains(section) },
                    perform: { model.setSidebarSection(section, hidden: !gui.hiddenSidebarSections.contains(section)) }
                )
                submenu.addItem(action.makeItem(section.title))
                continue
            }
            submenu.addItem(toggleItem(section.title, isOn: { !model.gui.hiddenSidebarSections.contains(section) }, toggle: {
                model.setSidebarSection(section, hidden: !model.gui.hiddenSidebarSections.contains(section))
            }))
        }
        item.submenu = submenu
        return item
    }

    private func toggleItem(_ title: String, isOn: @escaping @MainActor () -> Bool, toggle: @escaping @MainActor () -> Void) -> NSMenuItem {
        MenuItemAction(isEnabled: unlocked, isOn: isOn, perform: toggle).makeItem(title)
    }

    private var unlocked: @MainActor () -> Bool {
        { [model] in !model.lock.isLocked }
    }
}
