import SwiftUI

/// The RSS Reader window, kept for snapshots: the main window shows `RSSPane`.
struct RSSWindow: View {
    @Environment(AppModel.self) private var app
    @State private var model = RSSModel()
    /// Snapshots: open with the first article of this item selected.
    var initialSelection: String?

    var body: some View {
        @Bindable var model = model
        RSSPane(model: model)
            .searchable(text: $model.filter, placement: .toolbar, prompt: "Filter feed items…")
            .toolbar { RSSToolbar(model: model) }
            .frame(minWidth: FeatureDesign.Window.rssMin.width, minHeight: FeatureDesign.Window.rssMin.height)
            .navigationTitle(model.totalUnread > 0 ? String(localized: "RSS (\(model.totalUnread))") : AppWindowID.rss.title)
            .task(id: app.phase) {
                await model.attach(app.client)
                if let initialSelection {
                    model.selection = [initialSelection]
                    model.articleSelection = Set(model.articles.prefix(1).map(\.id))
                }
            }
            .task { await model.observe(app.events()) }
    }
}

/// The RSS reader: feed tree | articles | article preview, with its sheets
/// and alerts. The toolbar and the filter field belong to the host window;
/// in the main window the feeds are listed in the sidebar instead
/// (`showsFeedList` off).
struct RSSPane: View {
    @Environment(AppModel.self) private var app
    @Bindable var model: RSSModel
    var showsFeedList = true

    var body: some View {
        Group {
            if !showsFeedList, model.nodes.allSatisfy({ $0.kind == .unread }) {
                ContentUnavailableView {
                    Label("No Feeds", systemImage: Symbol.dotRadiowavesUpForward)
                } description: {
                    Text("Subscribe to an RSS feed to read its items and download torrents from it.")
                } actions: {
                    Button("New Subscription…") { model.sheet = .newSubscription(url: RSSFeedSheet.clipboardURL()) }
                    Button("New Folder…") { model.sheet = .newFolder }
                }
            } else {
                reader
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .sheet(item: $model.sheet) { sheet in
            RSSSheetView(sheet: sheet, model: model)
        }
        .alert(model.alert?.title ?? "", item: $model.alert) { _ in
            Button("OK") {}
        } message: { alert in
            Text(alert.message)
        }
        .confirmationDialog("Deletion confirmation", item: $model.pendingDeletion) { nodes in
            Button("Delete", role: .destructive) { Task { await model.delete(nodes) } }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Are you sure you want to delete the selected RSS feeds?")
        }
    }

    private var reader: some View {
        HSplitView {
            if showsFeedList {
                RSSFeedList(model: model)
                    .frame(minWidth: 200, idealWidth: 240, maxWidth: 360, maxHeight: .infinity)
            }
            RSSArticleList(model: model)
                .frame(minWidth: 260, idealWidth: 340, maxHeight: .infinity)
            RSSArticleView(item: model.currentArticle, showsFeed: model.showsFeedColumn) {
                if let item = model.currentArticle { model.download([item], app: app) }
            }
            .frame(minWidth: 300, maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}
