import AppKit

/// View › Sort By, like Mail's: the sort fields, then Ascending / Descending,
/// editing the same sort order as the toolbar's Sort By, the Command Palette
/// and the list's column headers.
extension MainMenuController {
    func sortMenuItem() -> NSMenuItem {
        let item = NSMenuItem(title: TransferSortMenu.title, action: nil, keyEquivalent: "")
        item.keyEquivalentModifierMask = []
        let menu = NSMenu(title: item.title)
        let model = model
        let list = model.list
        for column in TransferColumn.sortFields {
            let action = MenuItemAction(
                isEnabled: { !model.lock.isLocked },
                isOn: { list.sortColumn == column },
                perform: { list.sortColumn = column }
            )
            let field = action.makeItem(column.sortTitle)
            menu.addItem(field)
            // Shown only while queueing is on, like the Edit menu's queue items.
            if column == .queuePosition { queueItems.append(field) }
        }
        // A column the list's headers chose that is not one of the fields above.
        let other = MenuItemAction(isEnabled: { !model.lock.isLocked }, isOn: { true }, perform: {})
            .makeItem("")
        menu.addItem(other)
        // How names compare, shown while sorting by name.
        let namesSeparator = NSMenuItem.separator()
        menu.addItem(namesSeparator)
        var nameItems = [namesSeparator]
        for byTitle in [false, true] {
            let action = MenuItemAction(
                isEnabled: { !model.lock.isLocked },
                isOn: { list.sortsNamesByTitle == byTitle },
                perform: { list.sortsNamesByTitle = byTitle }
            )
            let item = action.makeItem(byTitle ? TransferSortMenu.titleTitle : TransferSortMenu.fullNameTitle)
            menu.addItem(item)
            nameItems.append(item)
        }
        menu.addItem(.separator())
        for ascending in [true, false] {
            let action = MenuItemAction(
                isEnabled: { !model.lock.isLocked },
                isOn: { list.sortAscending == ascending },
                perform: { list.sortAscending = ascending }
            )
            menu.addItem(action.makeItem(ascending ? TransferSortMenu.ascendingTitle : TransferSortMenu.descendingTitle))
        }
        let updater = SortMenuUpdater(list: list, otherItem: other, nameItems: nameItems)
        updater.menuNeedsUpdate(menu)
        menu.delegate = updater
        // The menu's delegate is weak; the item keeps it alive.
        item.representedObject = updater
        item.submenu = menu
        return item
    }
}
