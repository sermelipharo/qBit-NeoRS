import AppKit

/// Content tab context menu (features.md §2.7.3).
@MainActor
struct ContentContextMenu {
    let model: AppModel
    let item: TorrentItem
    let nodes: [FileNode]
    let apply: @MainActor ([FilePriority: [Int64]]) -> Void

    func makeMenu() -> NSMenu? {
        let menu = NSMenu.manual()
        let model = model
        let item = item
        let nodes = nodes
        let apply = apply
        let tree = model.properties.files
        if nodes.count == 1, let node = nodes.first {
            menu.addAction(String(localized: "Open"), symbol: Symbol.arrowUpForwardApp) { model.openContent(item, node) }
            menu.addAction(String(localized: "Open Containing Folder"), symbol: Symbol.folder) { model.revealContent(item, node) }
            menu.addAction(String(localized: "Rename…"), symbol: Symbol.pencil) { model.requestRenameContent(item, node) }
            menu.addDivider()
            menu.addSubmenu(String(localized: "Priority")) { submenu in
                for choice in FilePriorityChoice.selectable {
                    submenu.addAction(choice.title) { apply(tree.setPriority(choice, for: nodes)) }
                }
                submenu.addDivider()
                submenu.addAction(String(localized: "By Shown File Order")) { apply(tree.prioritizeByShownOrder(nodes)) }
            }
        } else if nodes.count > 1 {
            for choice in FilePriorityChoice.selectable {
                menu.addAction(choice.menuTitle) { apply(tree.setPriority(choice, for: nodes)) }
            }
            menu.addDivider()
            menu.addAction(String(localized: "Priority by Shown File Order")) { apply(tree.prioritizeByShownOrder(nodes)) }
        }
        return menu.items.isEmpty ? nil : menu
    }
}
