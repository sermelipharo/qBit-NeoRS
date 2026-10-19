import Foundation

/// One technical tag of a pinned torrent's name ("1080p", "S01E02"), styled
/// as on the app's cards: a system tint, filled or outlined.
struct WidgetNameChip: Codable, Hashable, Sendable {
    static let maxLength = 24

    /// The system colors the cards use for tag categories.
    enum Tint: String, Codable, Sendable {
        case red, orange, yellow, green, mint, teal, cyan, blue, indigo, purple, pink, brown, gray
    }

    var text: String
    var tint: Tint
    var isOutlined: Bool

    init(text: String, tint: Tint, isOutlined: Bool) {
        self.text = WidgetText.bounded(text, Self.maxLength)
        self.tint = tint
        self.isOutlined = isOutlined
    }

    enum CodingKeys: String, CodingKey {
        case text = "x", tint = "c", isOutlined = "o"
    }
}
