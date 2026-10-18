import SwiftUI

/// Status text, symbol and color, matching the app's transfer list.
extension WidgetTorrentState {
    var title: String {
        switch self {
        case .downloading: String(localized: "Downloading")
        case .stalledDownloading: String(localized: "Stalled")
        case .metadata: String(localized: "Downloading metadata")
        case .seeding, .stalledSeeding: String(localized: "Seeding")
        case .queued: String(localized: "Queued")
        case .checking: String(localized: "Checking")
        case .moving: String(localized: "Moving")
        case .stopped: String(localized: "Stopped")
        case .completed: String(localized: "Completed")
        case .error: String(localized: "Errored")
        case .missingFiles: String(localized: "Missing Files")
        }
    }

    var systemImage: String {
        switch self {
        case .downloading: "arrow.down"
        case .stalledDownloading: "arrow.down"
        case .metadata: "ellipsis"
        case .seeding, .stalledSeeding: "arrow.up"
        case .queued: "clock"
        case .checking: "arrow.trianglehead.2.clockwise.rotate.90"
        case .moving: "folder"
        case .stopped: "pause.fill"
        case .completed: "checkmark"
        case .error: "exclamationmark"
        case .missingFiles: "questionmark"
        }
    }

    var color: Color {
        switch self {
        case .downloading, .metadata, .checking, .moving: WidgetPalette.download
        case .stalledDownloading: WidgetPalette.download.opacity(0.6)
        case .seeding: WidgetPalette.upload
        case .stalledSeeding: WidgetPalette.upload.opacity(0.6)
        case .queued: .brown
        case .stopped: .gray
        case .completed: .purple
        case .error, .missingFiles: .red
        }
    }
}
