import Foundation

/// Hands the torrent name rules to the core, which serves them to the Web UI
/// (`api_names.rs`). Signatures match the generated bindings.
protocol TorrentNamesClient: AnyObject, Sendable {
    /// In memory and cheap: the enabled rules in matching order plus the
    /// name switches.
    func setTorrentNameRules(rules: TorrentNameRules)
}

extension App: TorrentNamesClient {}
