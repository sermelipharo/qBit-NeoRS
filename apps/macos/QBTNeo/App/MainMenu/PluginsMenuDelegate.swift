import AppKit

/// Fills the Plugins menu when it opens: "Manage Plugins…", then one item
/// per enabled plugin with an `invoke` function (qBittorrent 5.3's
/// `MainWindow::populatePluginsMenu`).
@MainActor
final class PluginsMenuDelegate: NSObject, NSMenuDelegate {
    private let model: AppModel
    /// Items below the separator, rebuilt on each opening.
    private var pluginItems: [NSMenuItem] = []

    init(model: AppModel) {
        self.model = model
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        for item in pluginItems { menu.removeItem(item) }
        pluginItems = model.invocablePlugins().map { plugin in
            let action = MenuItemAction(
                isEnabled: { [model] in !model.lock.isLocked },
                perform: { [model] in model.invokePlugin(plugin.id) }
            )
            return action.makeItem(plugin.name)
        }
        for item in pluginItems { menu.addItem(item) }
        // This menu has its own delegate, so it loads its symbols itself (`MenuSymbolLoader`).
        for item in menu.items where item.image == nil {
            guard let symbol = (item.target as? MenuItemAction)?.symbol else { continue }
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: nil)
        }
    }
}
