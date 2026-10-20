import Foundation

extension DemoClient: TorrentNamesClient {
    /// The demo has no Web UI to serve the rules to.
    func setTorrentNameRules(rules: TorrentNameRules) {}
}
