import Foundation

/// What the app was doing when it wrote the snapshot.
enum WidgetAppStatus: String, Codable, Hashable, Sendable {
    /// Running and publishing.
    case running
    /// Quit normally (speeds are zero, controls do nothing).
    case notRunning
    /// "Update widgets" is off in Settings.
    case updatesOff
    /// The UI is locked: no torrent names are published.
    case locked
}
