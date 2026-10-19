import Foundation

/// Properties panel pages (`PropertyTab` order).
enum PropertiesTab: Int, CaseIterable, Identifiable, Sendable {
    case general = 0
    case trackers = 1
    case peers = 2
    case httpSources = 3
    case content = 4
    case speed = 5
    /// The execution log (a qBit NeoRS extension; qBittorrent shows it in a tab of its own).
    case log = 6

    var id: Int { rawValue }

    var title: String {
        switch self {
        case .general: String(localized: "General")
        case .trackers: String(localized: "Trackers")
        case .peers: String(localized: "Peers")
        case .httpSources: String(localized: "HTTP Sources")
        case .content: String(localized: "Content")
        case .speed: String(localized: "Speed")
        case .log: String(localized: "Log")
        }
    }

    var systemImage: String {
        switch self {
        case .general: Symbol.info
        case .trackers: Symbol.antennaRadiowavesLeftAndRight
        case .peers: Symbol.person2
        case .httpSources: Symbol.globe
        case .content: Symbol.folder
        case .speed: Symbol.chartXyaxisLine
        case .log: Symbol.textAlignleft
        }
    }

    /// qBittorrent's Alt+letter shortcuts, used with ⌥ on macOS.
    var shortcutKey: Character {
        switch self {
        case .general: "g"
        case .trackers: "c"
        case .peers: "r"
        case .httpSources: "b"
        case .content: "z"
        case .speed: "d"
        case .log: "l"
        }
    }
}
