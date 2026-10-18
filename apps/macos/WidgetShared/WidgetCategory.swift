import Foundation

/// A category offered in the widget configuration, with its counts.
struct WidgetCategory: Codable, Hashable, Sendable, Identifiable {
    var name: String
    var counts: WidgetCounts

    var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name = "n", counts = "c"
    }
}
