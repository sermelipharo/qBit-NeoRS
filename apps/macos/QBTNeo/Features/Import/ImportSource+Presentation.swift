import Foundation

extension ImportSource {
    /// Both sources, in menu order.
    static let all: [ImportSource] = [.qBittorrent, .transmission]

    /// The application's name (not localized).
    var appName: String {
        switch self {
        case .qBittorrent: "qBittorrent"
        case .transmission: "Transmission"
        }
    }

    var bundleIdentifier: String {
        switch self {
        case .qBittorrent: "org.qbittorrent.qBittorrent"
        case .transmission: "org.m0k.transmission"
        }
    }

    /// File menu and Command Palette title.
    var commandTitle: String {
        switch self {
        case .qBittorrent: String(localized: "Import from qBittorrent…")
        case .transmission: String(localized: "Import from Transmission…")
        }
    }

    /// Stable sheet and command identifier part.
    var key: String {
        switch self {
        case .qBittorrent: "qbittorrent"
        case .transmission: "transmission"
        }
    }
}
