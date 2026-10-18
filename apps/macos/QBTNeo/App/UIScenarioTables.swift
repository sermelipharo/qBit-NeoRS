#if DEBUG
import AppKit
import SwiftUI

/// Scenario "tables" (for `--demo`): the AppKit tables of the main window,
/// exercised through real events and their own APIs: selection, sorting,
/// column visibility and its persisted layout, context menus, type-select,
/// Return, Delete, F2, and the Trackers, Peers and Content tabs.
extension UIScenarios {
    static func tables(_ d: UIDriver) async throws {
        try await waitRunning(d)
        _ = await d.wait(timeout: 20) { !d.model.list.visibleItems.isEmpty }
        d.log("windows: \(NSApp.windows.map { "\(type(of: $0))'\($0.title)' visible=\($0.isVisible)" }) main=\(d.model.mainWindow != nil)")
        _ = await d.wait(timeout: 10) { d.model.mainWindow != nil }
        var hosted: NSWindow?
        if d.model.mainWindow == nil {
            // Launched in the background through `open --env`, SwiftUI may not present the main window:
            // host the same view in a window that is never activated.
            let controller = NSHostingController(rootView: MainWindow().environment(d.model).environment(d.model.gui)
                .frame(width: 1_280, height: 800))
            let window = NSWindow(contentViewController: controller)
            window.styleMask = [.titled, .closable, .resizable, .fullSizeContentView]
            window.setContentSize(CGSize(width: 1_280, height: 800))
            window.isReleasedWhenClosed = false
            window.orderFrontRegardless()
            hosted = window
            _ = await d.wait(timeout: 10) { d.model.mainWindow != nil }
        }
        defer { hosted?.close() }
        let window = try mainWindow(d)
        await d.ensureKey(window)
        let table = try transferTable(d)
        let list = d.model.list
        d.check(table.numberOfRows == list.visibleItems.count, "transfer rows match the model (\(table.numberOfRows))")
        await d.pause(1)
        d.check(table.usesAlternatingRowBackgroundColors == d.model.gui.alternatingRowColors,
                "stripes follow the setting (\(table.usesAlternatingRowBackgroundColors) / \(d.model.gui.alternatingRowColors))")
        d.model.gui.alternatingRowColors.toggle()
        await d.pause(0.5)
        d.check(table.usesAlternatingRowBackgroundColors == d.model.gui.alternatingRowColors, "stripes follow a changed setting")
        d.model.gui.alternatingRowColors.toggle()

        // Selection both ways.
        await d.clickRow(table, row: 1)
        d.check(list.selection == [list.visibleItems[1].id], "click selects the row in the model")
        list.selection = [list.visibleItems[3].id]
        await d.pause(0.5)
        d.check(table.selectedRowIndexes == [3], "model selection reaches the table")

        // Sorting from the header.
        table.sortDescriptors = [NSSortDescriptor(key: TransferColumn.size.rawValue, ascending: false)]
        await d.pause(0.5)
        d.check(list.sortOrder.first == TransferSort(.size, order: .reverse), "header sort reaches the model")
        let sizes = list.visibleItems.map(\.size)
        d.check(sizes == sizes.sorted(by: >), "rows sorted by size, descending")
        d.check(table.selectedRowIndexes.count == 1 && list.selectedItems.count == 1
                    && list.visibleItems[table.selectedRow].id == list.selectedIDs.first,
                "selection follows its torrent after sorting")
        list.sortOrder = [TransferSort(.name)]
        await d.pause(0.5)
        d.check(table.sortDescriptors.first?.key == TransferColumn.name.rawValue, "model sort shows in the header")

        // Column visibility from the header menu, persisted.
        if let menu = table.headerView?.menu {
            menu.delegate?.menuNeedsUpdate?(menu)
            d.log("HEADER MENU: " + UIDriver.menuSummary(menu))
            if let index = menu.items.firstIndex(where: { $0.title == TransferColumn.ratio.title }) {
                let wasHidden = table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(TransferColumn.ratio.rawValue))?.isHidden
                menu.performActionForItem(at: index)
                await d.pause(1)
                let isHidden = table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(TransferColumn.ratio.rawValue))?.isHidden
                d.check(wasHidden != isHidden, "header menu toggles the Ratio column")
                let stored = TableColumnLayout.load(key: TransferTableController.layoutKey)
                d.check(stored?.columns.first { $0.id == TransferColumn.ratio.rawValue }?.isHidden == isHidden, "column layout persisted")
            } else {
                d.check(false, "header menu lists the Ratio column")
            }
        } else {
            d.check(false, "header has a column menu")
        }

        // Context menu of a row.
        _ = await d.contextMenuPerform(table: table, row: 2, path: ["Torrent Options…"])
        _ = await d.wait(timeout: 5) { d.model.sheet != nil }
        d.check(d.model.sheet != nil, "context menu item opens Torrent Options")
        d.model.sheet = nil
        await d.pause(0.8)

        // Keyboard: type-select, F2, Delete.
        window.makeFirstResponder(table)
        let target = list.visibleItems[list.visibleItems.count - 1]
        d.typeNow(String(target.name.prefix(4)), window: window)
        await d.pause(1.2)
        d.check(list.selection == [target.id], "type-select picks '\(target.name.prefix(4))…'")
        d.keyNow(String(Character(UnicodeScalar(UInt16(NSF2FunctionKey))!)), code: 120, window: window)
        await d.pause(0.8)
        d.check(d.model.sheet != nil, "F2 asks for a new name")
        d.model.sheet = nil
        await d.pause(0.8)
        d.keyNow("\u{7f}", code: 51, window: window)
        await d.pause(0.8)
        d.check(d.model.sheet != nil || d.model.confirmation != nil, "Delete asks to remove the torrent")
        d.model.sheet = nil
        d.model.confirmation = nil
        await d.pause(0.8)

        // Sidebar: one row per section, filters combine, collapsing resets.
        if let sidebar = d.views(in: window, classNameContains: "OutlineView").compactMap({ $0 as? NSOutlineView })
            .first(where: { $0.identifier?.rawValue == "Sidebar" }) {
            func row(_ item: SidebarItem) -> Int {
                (0 ..< sidebar.numberOfRows).first { (sidebar.item(atRow: $0) as? SidebarEntry)?.item == item } ?? -1
            }
            let downloading = row(.status(.downloading))
            await d.clickRow(sidebar, row: downloading, xOffset: 40)
            d.check(list.statusFilter == .downloading, "sidebar click sets the status filter")
            let all = row(.category(.all))
            sidebar.selectRowIndexes(IndexSet([downloading, all + 3]), byExtendingSelection: false)
            await d.pause(0.5)
            d.check(list.statusFilter == .downloading && list.categoryFilter != .all,
                    "a category combines with the status filter (\(list.categoryFilter))")
            d.check(sidebar.selectedRowIndexes.count == 2, "both section rows stay highlighted")
            _ = await d.contextMenuPerform(table: sidebar, row: all + 3, path: ["(log only)"])
            if let section = sidebar.item(atRow: row(.category(.all)) - 1) {
                sidebar.collapseItem(section)
                await d.pause(0.5)
                d.check(!d.model.gui.categorySectionExpanded && list.categoryFilter == .all, "collapsing Categories resets its filter")
                sidebar.expandItem(section)
                await d.pause(0.5)
                d.check(d.model.gui.categorySectionExpanded, "expanding Categories is remembered")
            }
            list.resetAllFilters()
            await d.pause(0.5)
        } else {
            d.check(false, "sidebar outline shown")
        }

        // Properties tables, for a multi-file torrent.
        let multiFile = list.visibleItems.first { $0.name.contains("Collection") || $0.name.contains("Grateful") } ?? list.visibleItems[3]
        list.selection = [multiFile.id]
        for (tab, identifier) in [(PropertiesTab.trackers, "TrackerList"), (.peers, "PeerList"), (.content, "ContentList")] {
            try await showTab(d, tab)
            let tables = d.views(in: window, classNameContains: "TableView").compactMap { $0 as? NSTableView }
            let found = tables.first { $0.identifier?.rawValue == identifier }
            d.check(found != nil, "\(tab.title) table shown")
            if let found {
                d.log("\(tab.title) rows=\(found.numberOfRows): \(texts(of: found, d: d, limit: 3))")
                d.check(found.numberOfRows > 0 || tab == .peers, "\(tab.title) table has rows")
            }
        }
        if let content = d.views(in: window, classNameContains: "TableView").compactMap({ $0 as? NSTableView })
            .first(where: { $0.identifier?.rawValue == "ContentList" }), content.numberOfRows > 0 {
            let tree = d.model.properties.files
            let rowsBefore = tree.rows.count
            d.log("content rows: \(tree.rows.map { "\($0.name)\($0.isFolder ? "/" : "")\($0.isExpanded ? "+" : "")" })")
            if let folder = tree.rows.firstIndex(where: { $0.isFolder && $0.isExpanded }) {
                await d.clickRow(content, row: folder, xOffset: 200)
                window.makeFirstResponder(content)
                d.keyNow(String(Character(UnicodeScalar(UInt16(NSLeftArrowFunctionKey))!)), code: 123, window: window)
                await d.pause(0.8)
                d.check(tree.rows.count < rowsBefore, "Left arrow collapses the folder (\(rowsBefore) -> \(tree.rows.count))")
                d.keyNow(String(Character(UnicodeScalar(UInt16(NSRightArrowFunctionKey))!)), code: 124, window: window)
                await d.pause(0.8)
                d.check(tree.rows.count == rowsBefore, "Right arrow expands it again")
            }
        }
        await d.shot("tables")
    }
}
#endif
