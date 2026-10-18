import Foundation

/// What one Transfers widget lists: a filter, optionally within one category.
struct WidgetScope: Hashable, Sendable {
    var filter: WidgetTorrentFilter
    var category: String?
}
