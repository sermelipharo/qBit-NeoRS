import Foundation

/// Fixed import data for snapshot mode and tests: nothing is read or written.
struct DemoImportClient: ImportClient {
    var running: Set<ImportSource> = []
    var profileHasTorrents = false

    func detect(_ source: ImportSource) -> ImportDetection {
        ImportDetection(
            source: source, found: true,
            location: source == .transmission ? "~/Library/Application Support/Transmission" : "~/Library/Application Support/qBittorrent",
            torrents: source == .transmission ? 24 : 57, bundleId: source.bundleIdentifier
        )
    }

    func plan(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws -> ImportPlan {
        Self.samplePlan(source, merge: keepSettings || profileHasTorrents, profileHasTorrents: profileHasTorrents)
    }

    static func samplePlan(_ source: ImportSource, merge: Bool, profileHasTorrents: Bool) -> ImportPlan {
        switch source {
        case .transmission:
            return ImportPlan(
                source: source, merge: merge, profileHasTorrents: profileHasTorrents,
                torrents: 24, torrentsWithPieces: 21, torrentsToCheck: 2, magnets: 1, torrentsMerged: 0, torrentsFailed: 0,
                categories: 3, tags: 2, watchedFolders: 1, rssFeeds: 0, searchPlugins: 0, guiSettings: 3, remoteAccessEnabled: true,
                notCarried: [
                    "Transmission remote access (RPC, port 9091, with a password): its password is not copied; "
                        + "qBit NeoRS offers the qBittorrent Web UI and WebAPI instead (Settings > Web UI)",
                    "automatic group assignment rules (assign categories by hand, or use RSS/watched folder rules)",
                    "known peers, per-torrent bandwidth priority and peer limit",
                ],
                warnings: [], report: "Transmission -> qBit NeoRS migration (dry run: nothing written)\n"
            )
        case .qBittorrent:
            return ImportPlan(
                source: source, merge: merge, profileHasTorrents: profileHasTorrents,
                torrents: 57, torrentsWithPieces: 55, torrentsToCheck: 1, magnets: 1, torrentsMerged: 0, torrentsFailed: 0,
                categories: 6, tags: 4, watchedFolders: 1, rssFeeds: 9, searchPlugins: 5, guiSettings: 31, remoteAccessEnabled: false,
                notCarried: [
                    "Qt-specific (window geometry, column layouts, styles): 18 setting(s)",
                    "cookies, partially downloaded pieces, known peers, logs",
                ],
                warnings: [], report: "qBittorrent -> qBit NeoRS migration (dry run: nothing written)\n"
            )
        }
    }

    func request(_ source: ImportSource, profileDirectory: String, keepSettings: Bool) throws {}

    func isRunning(_ source: ImportSource) -> Bool {
        running.contains(source)
    }
}
