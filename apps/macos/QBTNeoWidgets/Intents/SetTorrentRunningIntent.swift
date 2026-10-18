import AppIntents

/// Starts or stops one torrent in the running app (a Torrent widget button).
/// The request is queued as a file in the App Group container, because a
/// Darwin notification cannot carry the hash; the notification only tells
/// the app to look.
struct SetTorrentRunningIntent: AppIntent {
    static let title: LocalizedStringResource = "Start or Stop Torrent"
    static let description = IntentDescription("Starts or stops a torrent in qBit NeoRS.")
    /// Works only while the app runs, so it is not offered in Shortcuts.
    static let isDiscoverable = false

    @Parameter(title: "Torrent")
    var torrentID: String

    @Parameter(title: "Start")
    var start: Bool

    init() {}

    init(torrentID: String, start: Bool) {
        self.torrentID = torrentID
        self.start = start
    }

    func perform() async throws -> some IntentResult {
        guard WidgetText.isTorrentID(torrentID) else { return .result() }
        let command = WidgetTorrentCommand(torrentID: torrentID, action: start ? .start : .stop, issuedAt: .now)
        if WidgetCommandQueue.enqueue(command) { WidgetMessage.torrentCommand.post() }
        return .result()
    }
}
