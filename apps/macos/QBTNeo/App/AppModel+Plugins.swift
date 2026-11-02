import Foundation

extension AppModel {
    /// Enabled plugins with an `invoke` function, for the Plugins menu (an
    /// in-memory read in the core).
    func invocablePlugins() -> [PluginInfo] {
        guard let client = client as? any PluginsClient else { return [] }
        return client.pluginsStatus().plugins.filter { $0.invocable && $0.enabled }.sorted { $0.name < $1.name }
    }

    /// Plugins menu: calls the plugin's `invoke` function. Failures are in
    /// the execution log, as in qBittorrent.
    func invokePlugin(_ id: String) {
        guard let client = client as? any PluginsClient else { return }
        Task { try? await client.invokePlugin(id: id) }
    }
}
