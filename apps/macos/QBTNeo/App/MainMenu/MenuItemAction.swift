import AppKit

/// Target of a hand-built main menu item: runs a closure, and answers AppKit's
/// validation (before the menu opens and before a key equivalent fires) with
/// the item's current enabled and on/off state, so nothing is updated ahead of
/// time.
@MainActor
final class MenuItemAction: NSObject, NSMenuItemValidation {
    private let perform: @MainActor () -> Void
    private let isEnabled: @MainActor () -> Bool
    private let isOn: (@MainActor () -> Bool)?
    /// The item's SF Symbol, loaded when its menu first opens (`MenuSymbolLoader`).
    private(set) var symbol: String?

    init(
        isEnabled: @escaping @MainActor () -> Bool = { true },
        isOn: (@MainActor () -> Bool)? = nil,
        perform: @escaping @MainActor () -> Void
    ) {
        self.perform = perform
        self.isEnabled = isEnabled
        self.isOn = isOn
    }

    @objc func invoke(_ sender: Any?) {
        perform()
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if let isOn {
            let state: NSControl.StateValue = isOn() ? .on : .off
            if menuItem.state != state { menuItem.state = state }
        }
        return isEnabled()
    }

    /// A menu item running this action; the item keeps the action alive. Its
    /// symbol image is set when the menu first opens: creating a symbol image
    /// parses the symbol's vector data, which no shortcut needs.
    func makeItem(_ title: String, symbol: String? = nil, shortcut: CommandShortcut? = nil) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: #selector(invoke(_:)), keyEquivalent: "")
        item.keyEquivalentModifierMask = []
        item.target = self
        item.representedObject = self
        self.symbol = symbol
        if let shortcut {
            item.keyEquivalent = shortcut.menuKeyEquivalent
            item.keyEquivalentModifierMask = shortcut.menuModifierMask
        }
        return item
    }
}
