import Foundation

/// What the name parser found in one torrent name. Ranges are UTF-16
/// (`NSRange`) offsets into the original name, ready for attributed strings.
struct ParsedName: Equatable, Sendable {
    /// The title with separators normalized and technical tokens removed.
    let title: String
    /// Where the title is in the original name (shown semibold in the list).
    let titleRange: NSRange
    /// Ordered by category, then by position; no duplicates.
    let chips: [NameChip]
    /// Every recognized token (including hidden ones), sorted by position.
    let technicalRanges: [NSRange]
    /// What the torrent most likely contains (`ContentKindDetector`).
    var kind: ContentKind = .other

    /// A name that is all title (parsing off, or nothing recognized).
    static func plain(_ name: String, kind: ContentKind = .other) -> Self {
        Self(title: name, titleRange: NSRange(location: 0, length: (name as NSString).length), chips: [], technicalRanges: [], kind: kind)
    }
}
