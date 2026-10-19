import AppKit
import SwiftUI

/// The menu of the toolbar search field's magnifying glass on the Transfers
/// tab: what the filter matches (name, save path, trackers) and a plugin
/// search for the typed phrase. It replaces the scope bar `.searchScopes`
/// would show under the toolbar. The field itself stays SwiftUI's, so it
/// keeps the system look; this finds its `NSSearchToolbarItem`.
struct MainSearchFieldMenu: NSViewRepresentable {
    let model: AppModel

    func makeNSView(context: Context) -> NSView {
        NSView()
    }

    func updateNSView(_ view: NSView, context: Context) {
        // Read here so SwiftUI updates the menu when they change.
        let pane = model.mainPane
        let scope = model.list.searchScope
        let text = model.list.searchText
        DispatchQueue.main.async { [weak view] in
            guard let field = view?.window.flatMap(Self.searchItem)?.searchField else { return }
            field.searchMenuTemplate = pane == .transfers ? Self.menu(model: model, scope: scope, text: text) : nil
        }
    }

    static func searchItem(in window: NSWindow) -> NSSearchToolbarItem? {
        window.toolbar?.items.lazy.compactMap { $0 as? NSSearchToolbarItem }.first
    }

    private static func menu(model: AppModel, scope current: TransferSearchScope, text: String) -> NSMenu {
        let menu = NSMenu.manual()
        let header = NSMenuItem.sectionHeader(title: String(localized: "Filter By"))
        menu.addItem(header)
        for scope in TransferSearchScope.allCases {
            menu.addAction(scope.title, state: scope == current ? .on : .off) { model.list.searchScope = scope }
        }
        menu.addDivider()
        let phrase = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = phrase.isEmpty
            ? String(localized: "Search Plugins…")
            : String(localized: "searchField.searchPlugins", defaultValue: "Search Plugins for “\(phrase)”",
                     comment: "Toolbar search field menu: searches the installed search plugins for the typed phrase")
        menu.addAction(title, symbol: Symbol.magnifyingglass) {
            if phrase.isEmpty { model.show(pane: .search) } else { model.searchPlugins(for: phrase) }
        }
        return menu
    }
}

extension AppModel {
    /// Edit › Find (⌘F): puts the cursor in the toolbar search field of the main window.
    func focusSearchField() {
        guard let window = mainWindow, let item = MainSearchFieldMenu.searchItem(in: window) else { return }
        window.makeKeyAndOrderFront(nil)
        item.beginSearchInteraction()
    }
}
