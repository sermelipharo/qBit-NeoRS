import AppIntents

/// The Transfers widget's "Show" option.
enum TorrentFilterOption: String, AppEnum {
    case active, downloading, seeding, recent, all

    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Torrents"
    static let caseDisplayRepresentations: [Self: DisplayRepresentation] = [
        .active: DisplayRepresentation(title: "Active", image: .init(systemName: "arrow.up.arrow.down")),
        .downloading: DisplayRepresentation(title: "Downloading", image: .init(systemName: "arrow.down")),
        .seeding: DisplayRepresentation(title: "Seeding", image: .init(systemName: "arrow.up")),
        .recent: DisplayRepresentation(title: "Recent", image: .init(systemName: "clock")),
        .all: DisplayRepresentation(title: "All", image: .init(systemName: "tray.full")),
    ]

    var filter: WidgetTorrentFilter {
        switch self {
        case .active: .active
        case .downloading: .downloading
        case .seeding: .seeding
        case .recent: .recent
        case .all: .all
        }
    }
}
