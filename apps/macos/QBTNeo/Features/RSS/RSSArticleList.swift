import AppKit
import SwiftUI

/// Articles of the selected feeds/folders (newest first). Double-click or
/// Return downloads; the context menu offers download and open news URL.
struct RSSArticleList: View {
    let model: RSSModel
    @Environment(AppModel.self) private var app
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        ListTableView(gui: app.gui) { Self.makeController(model: model, app: app) }
            .overlay {
                if model.articles.isEmpty {
                    ContentUnavailableView(
                        model.filter.isEmpty ? "No Articles" : "No Matches", systemImage: Symbol.newspaper,
                        description: Text(model.filter.isEmpty ? "Select a feed or folder to see its articles." : "No article title matches the filter.")
                    )
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) {
                VStack(spacing: 0) {
                    if !app.corePreferences.rss.rssProcessingEnabled {
                        FeatureBanner(text: "Fetching of RSS feeds is disabled now! You can enable it in application settings.",
                                      actionTitle: "Settings…") { openSettings() }
                    }
                    Text("Torrents: (double-click to download)")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, Design.Spacing.medium)
                        .padding(.vertical, Design.Spacing.small)
                }
                .background(.bar)
            }
    }

    private static func makeController(model: RSSModel, app: AppModel) -> ListTableController<RSSArticleItem> {
        var configuration = ListTableController<RSSArticleItem>.Configuration(
            identifier: "RSSArticles",
            accessibilityLabel: String(localized: "Articles"),
            specs: [TableColumnSpec("article", String(localized: "Torrents"), minWidth: 100, idealWidth: 400, isHideable: false,
                                    isSortable: false)],
            layoutKey: nil,
            rows: { model.articles },
            selection: { model.articleSelection },
            setSelection: { model.articleSelection = $0 },
            content: { _, item in TableCellContent(item.article.title) }
        )
        configuration.showsHeader = false
        configuration.followsAlternatingRowsSetting = false
        configuration.rowHeight = { item, width in RSSArticleRow.height(item, width: width) }
        configuration.makeCell = { table, _, item in
            let cell = RSSArticleRow.make(in: table)
            cell.configure(item, showsFeed: model.showsFeedColumn)
            return cell
        }
        configuration.configureCell = { view, _, item in
            (view as? RSSArticleRow)?.configure(item, showsFeed: model.showsFeedColumn)
        }
        configuration.menu = { ids in
            let items = model.articles.filter { ids.contains($0.id) }
            let menu = NSMenu.manual()
            if items.contains(where: { !$0.article.torrentUrl.isEmpty }) {
                menu.addAction(String(localized: "Download torrent"), symbol: Symbol.arrowDownCircle) { model.download(items, app: app) }
            }
            if items.contains(where: { !$0.article.link.isEmpty }) {
                menu.addAction(String(localized: "Open news URL"), symbol: Symbol.safari) { model.openNewsURLs(items, app: app) }
            }
            return menu.items.isEmpty ? nil : menu
        }
        configuration.primaryAction = { ids in
            model.download(model.articles.filter { ids.contains($0.id) }, app: app)
        }
        let controller = ListTableController(configuration)
        controller.observe({ model.showsFeedColumn }, apply: { [weak controller] _ in controller?.refreshAll() })
        return controller
    }
}
