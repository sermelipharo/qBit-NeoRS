import Foundation

/// One technical tag of a torrent name ("1080p", "ENG Sub", "S01E02").
struct NameChip: Hashable, Sendable {
    let category: NameRuleCategory
    let text: String
}
