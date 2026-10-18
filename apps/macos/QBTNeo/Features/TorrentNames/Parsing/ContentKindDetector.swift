// Marker patterns are data; wrapping them would make them harder to read.
// swiftlint:disable line_length
import Foundation

/// Guesses what a torrent contains from its name and the tags the rules
/// found. Fixed markers (not the editable rules, so editing a rule never
/// changes a kind) are checked in order, the first that applies wins:
/// adult, repacked or current-console games, ROM collections, audiobooks,
/// comics, video (resolution, HDR, video codec or video source tags,
/// episodes without audio tags, course words), other games, books, music (audio tags
/// or music words), software (platform or version tags, installer words),
/// then the file extension. A torrent's file list is not used: it is not
/// loaded for torrents that are not selected. Runs once per name (the
/// result is cached with the parsed name).
struct ContentKindDetector: @unchecked Sendable {
    // `NSRegularExpression` is immutable and documented as thread-safe.
    static let shared = Self()

    private let adult = Self.compile(#"(?-i:XXX)|^[A-Za-z][A-Za-z0-9]{1,30}[ ._]\d{2}[ ._](?:0[1-9]|1[0-2])[ ._](?:0[1-9]|[12]\d|3[01])[ ._]"#)
    private let rom = Self.compile(#"No-?Intro|Redump|TOSEC|MAME|FBNeo|FinalBurn|RetroArch|(?-i:ROMs?|CHD)|ROM[ ._]?sets?|\[!\]|\((?:USA|Europe|Japan|World)(?:, ?[A-Z][a-z]+)*\)|\(Rev[ ._]?[0-9A-Z]{1,3}\)"#)
    private let audiobook = Self.compile(#"M4B|Audio[ ._-]?books?|Аудиокниг[аи]|Аудиоспектакль|читает|Narrated[ ._]by|Unabridged|Abridged"#)
    private let comic = Self.compile(#"CB[ZR7]|Manga|Манга|Manhwa|Манхва|Comics?|Комиксы?|Webtoon|Graphic[ ._]Novel|Scanlations?|(?-i:c)\d{3,4}"#)
    /// Repackers and current consoles: checked before ROM regions ("(USA) [NSP]" is a game).
    private let currentGame = Self.compile(#"FitGirl|DODI|xatab|ElAmigos|(?-i:CODEX|PLAZA|RUNE|TENOKE|SKIDROW|EMPRESS|DARKSiDERS|GOG|NSP|XCI|NSZ|PKG|NSW)|[\[(]Switch[\])]|Nintendo[ ._]Switch|Steam-?Rip|PS[45]|Xbox[ ._]?(?:One|Series)|Механики"#)
    private let game = Self.compile(#"PS[23]|PSP|PS[ ._]?Vita|Xbox|DLCs?|MULTi\d{1,2}|GOTY"#)
    /// Courses and lectures are videos even without video tags.
    private let videoWords = Self.compile(#"Udemy|Coursera|Pluralsight|Skillshare|Domestika|MasterClass|Skillbox|GeekBrains|Нетология|(?:Video[ ._])?Course|Курс|Lectures|Tutorials?|Видеоурок\w*|Documentary|DOCU"#)
    private let book = Self.compile(#"EPUB|FB2|MOBI|AZW3?|DjVu|e-?Books?|Книг[аи]|Библиотека|Сборник[ ._]книг|Собрание[ ._]сочинений|Complete[ ._]Works"#)
    private let music = Self.compile(#"Discography|Дискография|Album|Альбом|Soundtrack|(?-i:OST|EP|Single)|Сингл|Podcast|Подкаст|kbps|FLAC|ALAC|MP3|V0|Lossless|Hi-?Res|Vinyl"#)
    private let software = Self.compile(#"Linux|BSD|Ubuntu|Debian|Fedora|Windows|macOS|Portable|x64|x86(?:_64)?|amd64|arm64|aarch64|Setup|Installer|Multilingual|Pre-?Activated"#)

    /// Tags that only video names have (episodes count when there are no
    /// audio tags: podcasts have episodes too).
    private static let videoCategories: Set<NameRuleCategory> = [.resolution, .hdr, .videoCodec]
    /// Sources that music, comics and disc images share with video ("WEB"
    /// FLAC, vinyl rips, digital comics, DVD images of Linux distributions).
    private static let sharedSources: Set<String> = ["web", "vinyl", "cd", "digital", "dvd"]

    func kind(name: String, chips: [NameChip]) -> ContentKind {
        let text = name.utf16.count > NameParser.maxInputLength ? String((name as NSString).substring(to: NameParser.maxInputLength)) : name
        if Self.matches(adult, text) { return .adult }
        if Self.matches(currentGame, text) { return .game }
        if Self.matches(rom, text) { return .rom }
        if Self.matches(audiobook, text) { return .audiobook }
        if Self.matches(comic, text) || chips.contains(where: { $0.category == .format && ["CBZ", "CBR"].contains($0.text) }) { return .comic }
        let hasAudioTags = chips.contains { $0.category == .audio }
        if chips.contains(where: Self.isVideoTag) || (!hasAudioTags && chips.contains { $0.category == .seasonEpisode })
            || Self.matches(videoWords, text) { return .video }
        if Self.matches(game, text) { return .game }
        let hasSoftwareTags = chips.contains { $0.category == .platform || $0.category == .version }
        if Self.matches(book, text) || (!hasSoftwareTags && chips.contains { $0.category == .format && $0.text == "PDF" }) { return .book }
        if hasAudioTags || Self.matches(music, text) { return .music }
        if hasSoftwareTags || Self.matches(software, text) { return .software }
        return Self.kind(fileExtension: (text as NSString).pathExtension) ?? .other
    }

    private static func isVideoTag(_ chip: NameChip) -> Bool {
        videoCategories.contains(chip.category) || (chip.category == .source && !sharedSources.contains(chip.text.lowercased()))
    }

    /// The kind a file extension stands for, if it is a telling one.
    static func kind(fileExtension: String) -> ContentKind? {
        switch fileExtension.lowercased() {
        case "mkv", "mp4", "m4v", "avi", "mov", "wmv", "ts", "m2ts", "webm", "mpg", "vob": .video
        case "flac", "mp3", "m4a", "ogg", "opus", "wav", "ape", "aiff", "dsf": .music
        case "m4b": .audiobook
        case "epub", "fb2", "mobi", "azw3", "djvu": .book
        case "cbz", "cbr", "cb7": .comic
        case "nsp", "xci", "nsz", "pkg", "cia": .game
        case "chd", "rvz", "wbfs": .rom
        case "iso", "dmg", "exe", "msi", "img", "apk", "appimage": .software
        default: nil
        }
    }

    /// A kind for a category name ("Movies", "Музыка", "Linux ISOs/Debian-based"),
    /// used when the name tells nothing. Matches the start of any word.
    static func kind(categoryName: String) -> ContentKind? {
        let words = categoryName.lowercased().split { !$0.isLetter }
        for (kind, keys) in categoryWords where words.contains(where: { word in keys.contains { Self.word(word, matches: $0) } }) {
            return kind
        }
        return nil
    }

    /// A key matches words that start with it; "=key" only the word itself.
    private static func word(_ word: Substring, matches key: String) -> Bool {
        key.hasPrefix("=") ? word == key.dropFirst() : word.hasPrefix(key)
    }

    /// In order: "audiobooks" is checked before "books".
    private static let categoryWords: [(ContentKind, [String])] = [
        (.adult, ["adult", "=xxx"]),
        (.audiobook, ["audiobook", "аудиокниг"]),
        (.comic, ["comic", "manga", "комикс", "манга"]),
        (.rom, ["=rom", "=roms", "emulat", "эмулятор"]),
        (.game, ["game", "игр"]),
        (.book, ["book", "ebook", "книг", "литератур"]),
        (.music, ["music", "audio", "album", "podcast", "музык", "подкаст"]),
        (.video, ["movie", "film", "video", "series", "=tv", "show", "anime", "documentar", "cartoon", "course",
                  "фильм", "кино", "сериал", "аниме", "мульт", "видео", "документ", "курс"]),
        (.software, ["software", "=app", "=apps", "application", "program", "linux", "distro", "=iso", "=isos", "программ", "софт"]),
    ]

    // MARK: Matching

    private static func compile(_ pattern: String) -> NSRegularExpression? {
        try? NSRegularExpression(pattern: pattern, options: [.caseInsensitive])
    }

    /// Whether `regex` has a whole-token match (no letter or digit touches it) in `text`.
    private static func matches(_ regex: NSRegularExpression?, _ text: String) -> Bool {
        guard let regex else { return false }
        let units = text as NSString
        var found = false
        regex.enumerateMatches(in: text, options: [], range: NSRange(location: 0, length: units.length)) { result, _, stop in
            guard let result, result.range.length > 0, NameParser.isWholeToken(result.range, in: units) else { return }
            found = true
            stop.pointee = true
        }
        return found
    }
}
// swiftlint:enable line_length
