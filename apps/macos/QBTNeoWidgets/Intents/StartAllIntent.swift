import AppIntents

/// Asks the running app to start every torrent (a widget button).
struct StartAllIntent: AppIntent {
    static let title: LocalizedStringResource = "Start All Torrents"
    static let description = IntentDescription("Starts every torrent in qBit NeoRS.")
    /// Works only while the app runs, so it is not offered in Shortcuts.
    static let isDiscoverable = false

    func perform() async throws -> some IntentResult {
        WidgetMessage.startAll.post()
        return .result()
    }
}
