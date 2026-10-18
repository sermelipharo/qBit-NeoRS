import SwiftUI

/// The Execution Log window (View › Log, features.md §3.15): "General"
/// messages with a level filter and "Blocked IPs".
struct ExecutionLogWindow: View {
    @Environment(AppModel.self) private var app
    @Environment(GUIPreferences.self) private var gui
    @State private var model = ExecutionLogModel()

    var body: some View {
        @Bindable var model = model
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
                LogFilterBar(model: model, gui: gui, clear: clear)
            }
            .searchable(text: $model.query, placement: .toolbar, prompt: "Filter messages")
            .toolbar {
                ToolbarItem(placement: .principal) {
                    Picker("Log", selection: $model.tab) {
                        ForEach(ExecutionLogTab.allCases) { tab in
                            Text(tab.title).tag(tab)
                        }
                    }
                    .pickerStyle(.segmented)
                    .labelsHidden()
                }
            }
            .frame(minWidth: FeatureDesign.Window.logMin.width, minHeight: FeatureDesign.Window.logMin.height)
            .navigationTitle(AppWindowID.log.title)
            .task(id: LogFilterKey(lastID: app.log.lastID, count: app.log.messages.count, types: gui.logTypes, query: model.query)) {
                model.rebuild(from: app.log.messages, types: gui.logTypes)
            }
            .task(id: model.tab) {
                guard model.tab == .blockedIPs else { return }
                await model.pollBlockedPeers(client: app.client)
            }
    }

    private func clear() {
        model.clear(store: app.log)
    }
}
