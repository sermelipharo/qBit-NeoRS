import Foundation

/// Demo plugins: qBittorrent 5.3's example plugins, as the engine lists them.
struct DemoPluginsState {
    var plugins: [PluginInfo] = [
        Self.plugin("EmailNotifier", "Email Notifier Example", handlers: ["onTorrentFinished"], mails: 3),
        Self.plugin("EventHandler", "Event Handler Example",
                    handlers: ["onTorrentAdded", "onTorrentMetadataReady", "onTorrentFinished", "onTorrentStarted", "onTorrentStopped",
                               "onTorrentAboutToBeRemoved"], enabled: false),
        Self.plugin("Invocable", "Invocable Example", handlers: [], invocable: true),
        Self.plugin("RunExternalProgram", "Run External Program Example",
                    handlers: ["onTorrentAdded", "onTorrentFinished", "onTorrentAboutToBeRemoved"], programs: 12, failures: 1,
                    lastError: "RunExternalProgram.lua:12: attempt to index a nil value (local 'torrent')"),
    ]

    var status: PluginsStatus {
        PluginsStatus(running: true, folder: "~/Library/Application Support/qBit NeoRS/data/plugins",
                      plugins: plugins.sorted { $0.id < $1.id }, problems: [])
    }

    private static func plugin(
        _ id: String, _ name: String, handlers: [String], invocable: Bool = false, enabled: Bool = true,
        programs: Int64 = 0, mails: Int64 = 0, failures: Int64 = 0, lastError: String = ""
    ) -> PluginInfo {
        PluginInfo(
            id: id, name: name, version: "0.1", author: "", description: "", invocable: invocable, enabled: enabled,
            handlers: handlers, filePath: "~/Library/Application Support/qBit NeoRS/data/plugins/\(id).lua",
            fileSize: Int64(200 + id.count * 37), sha256: String(repeating: "5f3a9c", count: 10) + "0b1e",
            programsRun: programs, mailsSent: mails, failures: failures, lastError: lastError
        )
    }
}
