import AppKit

/// Building context menus of AppKit tables with closures, the way the
/// SwiftUI menus they replace were written (titles, SF Symbols, checked and
/// mixed states, disabled items, submenus, collapsing dividers).
@MainActor
extension NSMenu {
    /// A menu whose items are enabled exactly as built.
    static func manual() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        return menu
    }

    @discardableResult
    func addAction(
        _ title: String, symbol: String? = nil, state: NSControl.StateValue = .off, isEnabled: Bool = true,
        toolTip: String? = nil, action: @escaping @MainActor () -> Void
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: #selector(ClosureMenuTarget.invoke), keyEquivalent: "")
        let target = ClosureMenuTarget(action: action)
        item.target = target
        item.representedObject = target
        if let symbol { item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) }
        item.state = state
        item.isEnabled = isEnabled
        if let toolTip, !toolTip.isEmpty { item.toolTip = toolTip }
        addItem(item)
        return item
    }

    /// A toggle over several values: on when all are true, mixed when they differ.
    @discardableResult
    func addToggle(_ title: String, values: [Bool], toolTip: String? = nil, action: @escaping @MainActor (Bool) -> Void) -> NSMenuItem {
        let allOn = !values.isEmpty && values.allSatisfy(\.self)
        let state: NSControl.StateValue = allOn ? .on : (values.contains(true) ? .mixed : .off)
        // Choosing a mixed or unchecked toggle turns it on, like SwiftUI's `Toggle(sources:)`.
        return addAction(title, state: state, toolTip: toolTip) { action(!allOn) }
    }

    @discardableResult
    func addSubmenu(_ title: String, symbol: String? = nil, build: (NSMenu) -> Void) -> NSMenuItem {
        let submenu = Self.manual()
        build(submenu)
        submenu.trimSeparators()
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        if let symbol { item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil) }
        item.submenu = submenu
        addItem(item)
        return item
    }

    /// A divider, skipped at the top and after another divider.
    func addDivider() {
        guard let last = items.last, !last.isSeparatorItem else { return }
        addItem(.separator())
    }

    /// Removes a trailing divider (SwiftUI menus drop it too).
    func trimSeparators() {
        while let last = items.last, last.isSeparatorItem { removeItem(last) }
    }
}
