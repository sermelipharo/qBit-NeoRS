import Foundation

/// Decides which queued Torrent widget commands the app carries out (pure).
///
/// A command must name a torrent the app knows that an installed widget has
/// pinned, be recent (issued at most `maxAge` ago and not in the future), and
/// a torrent accepts at most one command per `spacing`. Of several commands
/// for one torrent only the newest counts.
struct WidgetCommandValidator: Sendable {
    static let maxAge: TimeInterval = 60
    static let futureTolerance: TimeInterval = 5
    static let spacing: TimeInterval = 1

    /// Monotonic time of the last accepted command per torrent.
    private var lastAccepted: [String: TimeInterval] = [:]

    init() {}

    /// - Parameters:
    ///   - now: wall-clock time, compared with `issuedAt`.
    ///   - uptime: monotonic time for the per-torrent spacing.
    ///   - allowed: torrents that exist and are pinned by an installed widget.
    mutating func accept(_ commands: [WidgetTorrentCommand], now: Date, uptime: TimeInterval,
                         allowed: Set<String>) -> [WidgetTorrentCommand] {
        var newest: [String: WidgetTorrentCommand] = [:]
        for command in commands {
            let age = now.timeIntervalSince(command.issuedAt)
            guard WidgetText.isTorrentID(command.torrentID), allowed.contains(command.torrentID),
                  age <= Self.maxAge, age >= -Self.futureTolerance else { continue }
            if let other = newest[command.torrentID], other.issuedAt > command.issuedAt { continue }
            newest[command.torrentID] = command
        }
        var accepted: [WidgetTorrentCommand] = []
        for (id, command) in newest.sorted(by: { $0.key < $1.key }) {
            if let last = lastAccepted[id], uptime - last < Self.spacing { continue }
            lastAccepted[id] = uptime
            accepted.append(command)
        }
        lastAccepted = lastAccepted.filter { uptime - $0.value < Self.spacing }
        return accepted
    }
}
