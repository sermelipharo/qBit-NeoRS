import Foundation

/// Lua plugin calls of the core facade (`api_plugins.rs`, qBittorrent 5.3's
/// plugins engine). Signatures match the generated bindings, so `App`
/// conforms without glue.
protocol PluginsClient: AnyObject, Sendable {
    func pluginsStatus() -> PluginsStatus
    func installPlugin(path: String) async throws -> PluginInstallResult
    func uninstallPlugin(id: String) async throws
    func setPluginEnabled(id: String, enabled: Bool) async throws
    func invokePlugin(id: String) async throws
}

extension App: PluginsClient {}
