import AppIntents

/// Asks the running app to stop every torrent (a widget button).
struct StopAllIntent: AppIntent {
    static let title: LocalizedStringResource = "Stop All Torrents"
    static let description = IntentDescription("Stops every torrent in qBit NeoRS.")
    /// Works only while the app runs, so it is not offered in Shortcuts.
    static let isDiscoverable = false

    func perform() async throws -> some IntentResult {
        WidgetMessage.stopAll.post()
        return .result()
    }
}
