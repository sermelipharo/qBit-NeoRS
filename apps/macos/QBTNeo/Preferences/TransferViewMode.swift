import Foundation

/// How the transfer list shows torrents (qBit NeoRS extension).
enum TransferViewMode: Int, CaseIterable, Identifiable, Sendable {
    case list = 0
    case cards = 1

    var id: Self { self }

    var title: String {
        switch self {
        case .list: String(localized: "viewMode.list", defaultValue: "as List", comment: "View menu: show torrents as a table")
        case .cards: String(localized: "viewMode.cards", defaultValue: "as Cards", comment: "View menu: show torrents as cards")
        }
    }

    /// Label of the toolbar's view picker and of Command Palette rows.
    var shortTitle: String {
        switch self {
        case .list: String(localized: "viewMode.list.short", defaultValue: "List", comment: "Transfer view mode")
        case .cards: String(localized: "viewMode.cards.short", defaultValue: "Cards", comment: "Transfer view mode")
        }
    }

    var symbol: String {
        switch self {
        case .list: Symbol.listBullet
        case .cards: Symbol.squareGrid2x2
        }
    }
}
