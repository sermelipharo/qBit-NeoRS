import Foundation
import Testing

struct WidgetCommandTests {
    private let first = "0123456789abcdef0123456789abcdef01234567"
    private let second = "fedcba9876543210fedcba9876543210fedcba98"
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func command(_ id: String, _ action: WidgetTorrentCommand.Action, age: TimeInterval = 0) -> WidgetTorrentCommand {
        WidgetTorrentCommand(torrentID: id, action: action, issuedAt: now.addingTimeInterval(-age))
    }

    @Test func acceptsOnlyAllowedRecentCommands() {
        var validator = WidgetCommandValidator()
        let accepted = validator.accept([
            command(first, .stop),
            command(second, .start), // not pinned
            command("ZZ", .start), // not a hash
            command(first.uppercased(), .start),
        ], now: now, uptime: 100, allowed: [first])
        #expect(accepted == [command(first, .stop)])

        var other = WidgetCommandValidator()
        #expect(other.accept([command(first, .stop, age: WidgetCommandValidator.maxAge + 1)], now: now, uptime: 0, allowed: [first]).isEmpty)
        #expect(other.accept([command(first, .stop, age: -60)], now: now, uptime: 0, allowed: [first]).isEmpty)
    }

    @Test func newestCommandPerTorrentWins() {
        var validator = WidgetCommandValidator()
        let accepted = validator.accept([command(first, .start, age: 1), command(first, .stop, age: 5), command(second, .stop)],
                                        now: now, uptime: 0, allowed: [first, second])
        #expect(accepted == [command(first, .start, age: 1), command(second, .stop)])
    }

    @Test func rateLimitsEachTorrent() {
        var validator = WidgetCommandValidator()
        #expect(validator.accept([command(first, .stop)], now: now, uptime: 10, allowed: [first]).count == 1)
        #expect(validator.accept([command(first, .start)], now: now, uptime: 10.5, allowed: [first]).isEmpty)
        #expect(validator.accept([command(second, .start)], now: now, uptime: 10.5, allowed: [second]).count == 1)
        #expect(validator.accept([command(first, .start)], now: now, uptime: 11, allowed: [first]).count == 1)
    }

    @Test func queueRoundTripsAndDrainsEverything() throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "widget-commands-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let sent = command(first, .start)
        #expect(WidgetCommandQueue.enqueue(sent, in: directory))
        // Garbage and oversized files are deleted unread.
        try Data("not json".utf8).write(to: directory.appending(path: "garbage.json"))
        try Data(repeating: 0x7B, count: WidgetCommandQueue.maxFileSize + 1).write(to: directory.appending(path: "big.json"))
        try FileManager.default.createDirectory(at: directory.appending(path: "folder"), withIntermediateDirectories: true)
        let drained = WidgetCommandQueue.drain(from: directory)
        #expect(drained.count == 1)
        #expect(drained.first?.torrentID == first)
        #expect(drained.first?.action == .start)
        #expect(try FileManager.default.contentsOfDirectory(atPath: directory.path(percentEncoded: false)).isEmpty)
        #expect(WidgetCommandQueue.drain(from: directory).isEmpty)
    }

    @Test func queueIgnoresSymbolicLinks() throws {
        let root = FileManager.default.temporaryDirectory.appending(path: "widget-commands-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let outside = root.appending(path: "outside")
        let queue = root.appending(path: "queue")
        #expect(WidgetCommandQueue.enqueue(command(first, .stop), in: outside))
        try FileManager.default.createDirectory(at: queue, withIntermediateDirectories: true)
        let target = try #require(try FileManager.default.contentsOfDirectory(at: outside, includingPropertiesForKeys: nil).first)
        try FileManager.default.createSymbolicLink(at: queue.appending(path: "link.json"), withDestinationURL: target)
        #expect(WidgetCommandQueue.drain(from: queue).isEmpty)
        // The link is removed, not the file it pointed to.
        #expect(FileManager.default.fileExists(atPath: target.path(percentEncoded: false)))
    }

    @Test func queueIsBounded() {
        let directory = FileManager.default.temporaryDirectory.appending(path: "widget-commands-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: directory) }
        let results = (0..<(WidgetCommandQueue.maxPending + 5)).map { _ in WidgetCommandQueue.enqueue(command(first, .stop), in: directory) }
        #expect(results.filter(\.self).count == WidgetCommandQueue.maxPending)
        #expect(WidgetCommandQueue.drain(from: directory).count == WidgetCommandQueue.maxPending)
    }
}
