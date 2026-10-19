import SwiftUI

/// The Execution Log window, kept for snapshots: the main window shows the
/// log as a bottom panel tab or full size (`LogPane`).
struct ExecutionLogWindow: View {
    @Environment(AppModel.self) private var app
    @Environment(GUIPreferences.self) private var gui
    @State private var model = ExecutionLogModel()

    var body: some View {
        @Bindable var model = model
        LogPane(model: model)
            .searchable(text: $model.query, placement: .toolbar, prompt: "Filter messages")
            .toolbar {
                ToolbarItem(placement: .principal) { LogTabPicker(model: model) }
            }
            .frame(minWidth: FeatureDesign.Window.logMin.width, minHeight: FeatureDesign.Window.logMin.height)
            .navigationTitle(AppWindowID.log.title)
            .modifier(LogFeed(model: model))
    }
}

/// General messages / Blocked IPs.
struct LogTabPicker: View {
    @Bindable var model: ExecutionLogModel

    var body: some View {
        Picker("Log", selection: $model.tab) {
            ForEach(ExecutionLogTab.allCases) { tab in
                Text(tab.title).tag(tab)
            }
        }
        .pickerStyle(.segmented)
        .labelsHidden()
        .fixedSize()
    }
}

/// The log table with its filter bar. `resize` adds the button that moves
/// it between the bottom panel and the full-size pane of the main window.
struct LogPane: View {
    @Environment(AppModel.self) private var app
    @Environment(GUIPreferences.self) private var gui
    let model: ExecutionLogModel
    var resize: LogFilterBar.Resize?

    var body: some View {
        ListTableView(gui: gui) { LogTable.makeController(model: model, clear: clear) }
            .overlay {
                if model.visibleLines.isEmpty {
                    ContentUnavailableView(
                        model.tab == .general ? "No Messages" : "No Blocked IPs",
                        systemImage: model.tab == .general ? Symbol.textAlignleft : Symbol.handRaised,
                        description: Text(model.query.isEmpty ? "New entries appear here as they are logged." : "No messages match the search.")
                    )
                }
            }
            .safeAreaInset(edge: .bottom, spacing: 0) {
                LogFilterBar(model: model, gui: gui, clear: clear, resize: resize)
            }
    }

    private func clear() {
        model.clear(store: app.log)
    }
}

/// Keeps the log's lines in step with the store and polls blocked peers on that tab.
struct LogFeed: ViewModifier {
    @Environment(AppModel.self) private var app
    @Environment(GUIPreferences.self) private var gui
    let model: ExecutionLogModel

    func body(content: Content) -> some View {
        content
            .task(id: LogFilterKey(lastID: app.log.lastID, count: app.log.messages.count, types: gui.logTypes, query: model.query)) {
                model.rebuild(from: app.log.messages, types: gui.logTypes)
            }
            .task(id: model.tab) {
                guard model.tab == .blockedIPs else { return }
                await model.pollBlockedPeers(client: app.client)
            }
    }
}
