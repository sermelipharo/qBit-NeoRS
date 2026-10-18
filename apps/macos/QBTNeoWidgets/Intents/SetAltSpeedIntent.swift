import AppIntents

/// Turns alternative speed limits on or off in the running app (a widget
/// button). The shared snapshot is updated right away, so the widget shows
/// the new state without waiting for the app's next write.
struct SetAltSpeedIntent: AppIntent {
    static let title: LocalizedStringResource = "Set Alternative Speed Limits"
    static let description = IntentDescription("Turns alternative speed limits on or off in qBit NeoRS.")
    /// Works only while the app runs, so it is not offered in Shortcuts.
    static let isDiscoverable = false

    @Parameter(title: "Enabled")
    var enabled: Bool

    init() {}

    init(enabled: Bool) {
        self.enabled = enabled
    }

    func perform() async throws -> some IntentResult {
        (enabled ? WidgetMessage.altSpeedOn : .altSpeedOff).post()
        if var snapshot = WidgetSnapshotStore.load(), snapshot.acceptsCommands {
            snapshot.altSpeedEnabled = enabled
            try? WidgetSnapshotStore.save(snapshot)
        }
        return .result()
    }
}
