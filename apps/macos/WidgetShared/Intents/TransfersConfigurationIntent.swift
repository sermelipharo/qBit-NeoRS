import AppIntents
import WidgetKit

/// The Transfers widget's options: which torrents, optionally one category.
/// Also compiled into the app, which reads installed configurations to know
/// which categories to publish.
struct TransfersConfigurationIntent: WidgetConfigurationIntent {
    static let title: LocalizedStringResource = "Transfers"
    static let description = IntentDescription("Choose which torrents the widget lists.")
    static let isDiscoverable = false

    @Parameter(title: "Show", default: .active)
    var filter: TorrentFilterOption

    @Parameter(title: "Category")
    var category: CategoryEntity?

    init() {}

    init(filter: TorrentFilterOption, category: String? = nil) {
        self.filter = filter
        self.category = category.map(CategoryEntity.init(id:))
    }

    var scope: WidgetScope {
        WidgetScope(filter: filter.filter, category: category?.id)
    }
}
