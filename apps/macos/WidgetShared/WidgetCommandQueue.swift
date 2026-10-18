import Foundation

/// The command queue: one small JSON file per command in
/// `<group>/commands/`, written atomically by the widget extension and
/// drained (read, then deleted) by the app when `WidgetMessage.torrentCommand`
/// arrives. Bounded on both sides; anything malformed is discarded.
enum WidgetCommandQueue {
    static let directoryName = "commands"
    /// The widget stops queueing beyond this many pending files.
    static let maxPending = 16
    /// Larger files are deleted unread.
    static let maxFileSize = 1_024

    static var directoryURL: URL? {
        WidgetSnapshotStore.containerURL?.appending(path: directoryName, directoryHint: .isDirectory)
    }

    /// Queues a command (widget side). Returns false when the queue is full or unwritable.
    @discardableResult
    static func enqueue(_ command: WidgetTorrentCommand, in directory: URL? = directoryURL) -> Bool {
        guard let directory else { return false }
        let manager = FileManager.default
        try? manager.createDirectory(at: directory, withIntermediateDirectories: true)
        let pending = (try? manager.contentsOfDirectory(atPath: directory.path(percentEncoded: false)))?.count ?? 0
        guard pending < maxPending else { return false }
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .secondsSince1970
        guard let data = try? encoder.encode(command) else { return false }
        let file = directory.appending(path: UUID().uuidString + ".json")
        return (try? data.write(to: file, options: [.atomic])) != nil
    }

    /// Reads and deletes every queued file (app side). At most `maxPending`
    /// commands are returned; the rest is discarded.
    static func drain(from directory: URL? = directoryURL) -> [WidgetTorrentCommand] {
        guard let directory else { return [] }
        let manager = FileManager.default
        let keys: [URLResourceKey] = [.isRegularFileKey, .fileSizeKey]
        guard let files = try? manager.contentsOfDirectory(at: directory, includingPropertiesForKeys: keys, options: [.skipsHiddenFiles])
        else { return [] }
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .secondsSince1970
        var commands: [WidgetTorrentCommand] = []
        for file in files.sorted(by: { $0.lastPathComponent < $1.lastPathComponent }) {
            defer { try? manager.removeItem(at: file) }
            guard commands.count < maxPending,
                  let values = try? file.resourceValues(forKeys: Set(keys)), values.isRegularFile == true,
                  let size = values.fileSize, size <= maxFileSize,
                  let data = try? Data(contentsOf: file),
                  let command = try? decoder.decode(WidgetTorrentCommand.self, from: data) else { continue }
            commands.append(command)
        }
        return commands
    }
}
