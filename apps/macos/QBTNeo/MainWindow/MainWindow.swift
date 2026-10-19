import SwiftUI

/// The main window: the sidebar (tabs on top, then the filters) and the
/// selected tab filling the rest: transfers with the properties panel and
/// status bar, plugin search, the RSS reader or the full-size execution log
/// (features.md §1). The toolbar and its search field follow the tab.
struct MainWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(GUIPreferences.self) private var gui
    @Environment(\.openWindow) private var openWindow
    @Environment(\.openSettings) private var openSettings

    var body: some View {
        @Bindable var gui = gui
        Group {
            if model.lock.isLocked {
                // Nothing of the session stays in the view tree while locked.
                UILockScreen()
            } else {
                NavigationSplitView(columnVisibility: $gui.sidebarVisibility) {
                    SidebarView()
                        .navigationSplitViewColumnWidth(
                            min: Design.Size.sidebarMinWidth, ideal: Design.Size.sidebarIdealWidth, max: Design.Size.sidebarMaxWidth
                        )
                } detail: {
                    detail
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                .toolbar(id: "main") { MainToolbar(model: model) }
                .searchable(text: searchText, placement: .toolbar, prompt: Text(searchPrompt))
                .onSubmit(of: .search) {
                    if model.mainPane == .search { model.searchTab.search() }
                }
                .background { MainSearchFieldMenu(model: model) }
            }
        }
        .modifier(MainWindowTitle())
        .modifier(MainPresentations())
        .modifier(MainTabFeeds(model: model))
        .overlay { StartupOverlay() }
        .background {
            WindowObserver(onVisibilityChange: { model.isMainWindowVisible = $0 || model.launch.assumesVisibleWindow }, onWindow: { model.mainWindow = $0 })
        }
        .onAppear {
            model.openWindow = openWindow
            model.openSettings = openSettings
        }
        .onChange(of: gui.filtersSidebarVisible) { _, visible in
            if !visible { model.list.resetSidebarFilters() }
        }
        .frame(minWidth: Design.Window.mainMinSize.width, minHeight: Design.Window.mainMinSize.height)
    }

    @ViewBuilder private var detail: some View {
        switch model.mainPane {
        case .transfers: TransferDetailView()
        case .search: SearchPane(model: model.searchTab, showsPatternField: false)
        case .rss: RSSPane(model: model.rssTab, showsFeedList: false)
        case .log:
            LogPane(model: model.logTab, resize: LogFilterBar.Resize(isFull: true) {
                gui.propertiesTab = .log
                gui.propertiesVisible = true
                model.mainPane = .transfers
            })
        }
    }

    /// One field, four uses: the transfers filter, the search phrase, the RSS filter and the log filter.
    private var searchText: Binding<String> {
        Binding {
            switch model.mainPane {
            case .transfers: model.list.searchText
            case .search: model.searchTab.pattern
            case .rss: model.rssTab.filter
            case .log: model.logTab.query
            }
        } set: { text in
            switch model.mainPane {
            case .transfers: model.list.searchText = text
            case .search: model.searchTab.pattern = text
            case .rss: model.rssTab.filter = text
            case .log: model.logTab.query = text
            }
        }
    }

    private var searchPrompt: String {
        switch model.mainPane {
        case .transfers: model.list.searchScope.prompt
        case .search: String(localized: "Search torrents with plugins…")
        case .rss: String(localized: "Filter feed items…")
        case .log: String(localized: "Filter messages")
        }
    }
}

/// Keeps the tabs' models fed whether or not their tab shows: plugin search
/// attaches to the core once enabled, RSS always (its feeds are listed in
/// the sidebar), the log follows the log store.
private struct MainTabFeeds: ViewModifier {
    let model: AppModel

    func body(content: Content) -> some View {
        content
            .modifier(LogFeed(model: model.logTab))
            .task(id: SearchAttachKey(phase: model.phase, enabled: model.gui.searchEnabled)) {
                guard model.gui.searchEnabled else { return }
                await model.searchTab.attach(model.client, profileRoot: model.launch.isSnapshot ? nil : model.launch.profileDirectory)
                runPendingPluginSearch()
            }
            .task(id: model.phase) {
                await model.rssTab.attach(model.client)
            }
            .task {
                await model.rssTab.observe(model.events())
            }
            .onChange(of: model.pendingPluginSearch) { runPendingPluginSearch() }
    }

    private func runPendingPluginSearch() {
        guard let pattern = model.pendingPluginSearch, !model.searchTab.plugins.isEmpty else { return }
        model.pendingPluginSearch = nil
        model.searchTab.pattern = pattern
        model.searchTab.search()
    }

    private struct SearchAttachKey: Equatable {
        let phase: AppModel.Phase
        let enabled: Bool
    }
}
