import AppKit

/// Delegate of the main menu's submenus: gives items their symbol images the
/// first time their menu opens (or is read by accessibility), instead of
/// parsing some fifty symbols at launch for a menu that may never open.
@MainActor
final class MenuSymbolLoader: NSObject, NSMenuDelegate {
    func menuNeedsUpdate(_ menu: NSMenu) {
        for item in menu.items where item.image == nil {
            guard let symbol = (item.target as? MenuItemAction)?.symbol else { continue }
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        }
    }

    /// Becomes the delegate of the submenus of `items`, recursively, except
    /// `excluded` (the Services menu, which AppKit fills).
    func adopt(_ items: [NSMenuItem], except excluded: NSMenu) {
        for item in items {
            guard let submenu = item.submenu, submenu !== excluded else { continue }
            if submenu.delegate == nil { submenu.delegate = self }
            adopt(submenu.items, except: excluded)
        }
    }
}
