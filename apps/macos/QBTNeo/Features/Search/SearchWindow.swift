import SwiftUI

/// The Search window, kept for snapshots: the main window shows `SearchPane`.
struct SearchWindow: View {
    @Environment(AppModel.self) private var app
    @State private var model = SearchModel()
    /// Snapshots: run this search on open.
    var initialPattern: String?

    var body: some View {
        SearchPane(model: model)
            .frame(minWidth: FeatureDesign.Window.searchMin.width, minHeight: FeatureDesign.Window.searchMin.height)
            .navigationTitle(AppWindowID.search.title)
            .task(id: app.phase) {
                await model.attach(app.client, profileRoot: app.launch.isSnapshot ? nil : app.launch.profileDirectory)
                if let initialPattern, model.tabs.isEmpty {
                    model.pattern = initialPattern
                    model.search()
                }
            }
    }
}

/// Search with the installed plugins: category and plugin pickers, result
/// tabs and the "Search plugins…" button. In the main window the phrase is
/// typed in the toolbar's search field (`showsPatternField` off).
struct SearchPane: View {
    @Environment(AppModel.self) private var app
    @Environment(\.openWindow) private var openWindow
    @Bindable var model: SearchModel
    var showsPatternField = true
    @AppStorage(FeatureDefaults.storeSearchTabs, store: AppDefaults.store) private var storesTabs = false
    @AppStorage(FeatureDefaults.storeSearchResults, store: AppDefaults.store) private var storesResults = false

    var body: some View {
        VStack(spacing: 0) {
            SearchQueryBar(model: model, showsPatternField: showsPatternField)
            Divider()
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack {
                Text(model.currentTab?.status.text ?? "")
                    .foregroundStyle(.secondary)
                Spacer()
                // The main window has it in the toolbar.
                if showsPatternField {
                    Button("Search plugins…") { openWindow(id: FeatureWindowID.searchPlugins.rawValue) }
                }
            }
            .padding(Design.Spacing.medium)
            .background(.bar)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .alert(model.alert?.title ?? "", item: $model.alert) { _ in
            Button("OK") {}
        } message: { alert in
            Text(alert.message)
        }
        .task {
            for await event in app.events() {
                if case .searchPluginsUpdated = event { await model.reloadPlugins() }
            }
        }
        .onChange(of: storesTabs) { model.storageSettingsChanged() }
        .onChange(of: storesResults) { model.storageSettingsChanged() }
    }

    @ViewBuilder private var content: some View {
        if let status = model.engineStatus, !status.pythonFound {
            ContentUnavailableView {
                Label("Missing Python Runtime", systemImage: Symbol.exclamationmarkTriangle)
            } description: {
                Text("Python is required to use the search engine but it does not seem to be installed.")
                Text(status.message)
                Text("Install Python 3.9 or later (for example from python.org or with Homebrew), or set its path in Settings › Advanced.")
            }
        } else if model.engineStatus != nil, model.plugins.isEmpty {
            ContentUnavailableView {
                Label("No Search Plugins", systemImage: Symbol.puzzlepieceExtension)
            } description: {
                Text("There aren't any search plugins installed.\nClick the \"Search plugins...\" button at the bottom right of the window to install some.")
            }
        } else if model.tabs.isEmpty {
            ContentUnavailableView("Search", systemImage: Symbol.magnifyingglass,
                                   description: Text("Type a phrase and press Return to search with the enabled plugins."))
        } else {
            VStack(spacing: 0) {
                SearchTabStrip(model: model)
                if let tab = model.currentTab {
                    SearchResultsView(model: model, tab: tab)
                        .id(tab.id)
                }
            }
        }
    }
}
