import Foundation

/// What a torrent most likely contains, guessed from its name
/// (`ContentKindDetector`) or its category; cards show it as a symbol.
enum ContentKind: String, CaseIterable, Sendable {
    case video
    case music
    case software
    case game
    case rom
    case audiobook
    case book
    case comic
    case adult
    case other

    var symbol: String {
        switch self {
        case .video: Symbol.film
        case .music: Symbol.musicNote
        case .software: Symbol.appBadge
        case .game: Symbol.gamecontroller
        case .rom: Symbol.memorychip
        case .audiobook: Symbol.headphones
        case .book: Symbol.book
        case .comic: Symbol.bookPages
        case .adult: Symbol.eighteenCircle
        case .other: Symbol.doc
        }
    }

    var title: String {
        switch self {
        case .video: String(localized: "names.kind.video", defaultValue: "Video", comment: "Kind of torrent content")
        case .music: String(localized: "names.kind.music", defaultValue: "Music", comment: "Kind of torrent content")
        case .software: String(localized: "names.kind.software", defaultValue: "Software", comment: "Kind of torrent content")
        case .game: String(localized: "names.kind.game", defaultValue: "Game", comment: "Kind of torrent content")
        case .rom: String(localized: "names.kind.rom", defaultValue: "ROM Collection",
                          comment: "Kind of torrent content: game ROMs or disc images for emulators")
        case .audiobook: String(localized: "names.kind.audiobook", defaultValue: "Audiobook", comment: "Kind of torrent content")
        case .book: String(localized: "names.kind.book", defaultValue: "Book", comment: "Kind of torrent content")
        case .comic: String(localized: "names.kind.comic", defaultValue: "Comic", comment: "Kind of torrent content: comics or manga")
        case .adult: String(localized: "names.kind.adult", defaultValue: "Adult", comment: "Kind of torrent content: adults only")
        case .other: String(localized: "names.kind.other", defaultValue: "Other", comment: "Kind of torrent content: unknown")
        }
    }
}
