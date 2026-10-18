import Foundation

/// What a name rule recognizes. Each category has one fixed look
/// (`NameChipStyle`), so "1080p" always looks the same and subtitles never
/// look like an audio language. Declaration order is the order chips appear in.
enum NameRuleCategory: String, CaseIterable, Codable, Sendable, Identifiable {
    case seasonEpisode
    /// Volumes, chapters, issues and discs (books, comics, music, ROMs).
    case volume
    case resolution
    case hdr
    case source
    case videoCodec
    case audio
    case audioLanguage
    case subtitles
    case edition
    case platform
    case version
    case region
    /// File formats of books, comics, audiobooks and game or disc images.
    case format
    case year
    case releaseGroup
    case other
    /// Removed from the title without a tag (file extensions, checksums, site names).
    case hidden

    var id: Self { self }

    /// Position among the chips of one name.
    var order: Int { Self.allCases.firstIndex(of: self) ?? 0 }

    /// Whether matches of this category become tags.
    var makesChip: Bool { self != .hidden }

    var title: String {
        switch self {
        case .seasonEpisode: String(localized: "names.category.seasonEpisode", defaultValue: "Season & Episode",
                                    comment: "Torrent name tag category")
        case .volume: String(localized: "names.category.volume", defaultValue: "Volume & Part", comment: "Torrent name tag category")
        case .resolution: String(localized: "names.category.resolution", defaultValue: "Resolution", comment: "Torrent name tag category")
        case .hdr: String(localized: "names.category.hdr", defaultValue: "HDR & Dolby Vision", comment: "Torrent name tag category")
        case .videoCodec: String(localized: "names.category.videoCodec", defaultValue: "Video Codec", comment: "Torrent name tag category")
        case .source: String(localized: "names.category.source", defaultValue: "Source", comment: "Torrent name tag category")
        case .audio: String(localized: "names.category.audio", defaultValue: "Audio", comment: "Torrent name tag category")
        case .audioLanguage: String(localized: "names.category.audioLanguage", defaultValue: "Language & Dub",
                                    comment: "Torrent name tag category")
        case .subtitles: String(localized: "names.category.subtitles", defaultValue: "Subtitles", comment: "Torrent name tag category")
        case .edition: String(localized: "names.category.edition", defaultValue: "Edition", comment: "Torrent name tag category")
        case .platform: String(localized: "names.category.platform", defaultValue: "Platform", comment: "Torrent name tag category")
        case .version: String(localized: "names.category.version", defaultValue: "Version", comment: "Torrent name tag category")
        case .region: String(localized: "names.category.region", defaultValue: "Region", comment: "Torrent name tag category")
        case .format: String(localized: "names.category.format", defaultValue: "Format", comment: "Torrent name tag category")
        case .year: String(localized: "names.category.year", defaultValue: "Year", comment: "Torrent name tag category")
        case .releaseGroup: String(localized: "names.category.releaseGroup", defaultValue: "Release Group", comment: "Torrent name tag category")
        case .other: String(localized: "names.category.other", defaultValue: "Other", comment: "Torrent name tag category")
        case .hidden: String(localized: "names.category.hidden", defaultValue: "Hidden (no tag)", comment: "Torrent name tag category")
        }
    }

    /// Decodes unknown future categories as `.other` instead of failing.
    init(from decoder: any Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        self = Self(rawValue: raw) ?? .other
    }
}
