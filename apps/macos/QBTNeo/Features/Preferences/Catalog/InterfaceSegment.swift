import Foundation

/// The inner segments of the Interface pane. Torrent names got a segment
/// rather than a ninth pane: nine panes overflow the Russian toolbar
/// (settings-ia.md §11.4).
enum InterfaceSegment: String, CaseIterable, Identifiable {
    case mainWindow
    case torrentNames

    var id: Self { self }

    var title: String {
        switch self {
        case .mainWindow: String(localized: "settings.interface.mainWindow", defaultValue: "Main Window", comment: "Settings › Interface segment")
        case .torrentNames: String(localized: "settings.interface.torrentNames", defaultValue: "Torrent Names", comment: "Settings › Interface segment")
        }
    }
}
