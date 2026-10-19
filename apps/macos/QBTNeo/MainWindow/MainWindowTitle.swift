import SwiftUI

/// Window title (with speeds when "Speed in Title Bar" is on) and the
/// "Transfers (N)" subtitle, isolated so per-tick title changes do not
/// re-evaluate the whole window body.
struct MainWindowTitle: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        content
            .navigationTitle(model.lock.isLocked ? AppWindowID.main.title : model.windowTitle)
            .navigationSubtitle(model.lock.isLocked ? Text(verbatim: "") : subtitle)
    }

    private var subtitle: Text {
        switch model.mainPane {
        case .transfers: Text("Transfers (\(model.transfers.count))")
        case .search: Text("Search")
        case .rss:
            if model.rssUnread > 0 { Text("RSS (\(model.rssUnread))") } else { Text("RSS") }
        case .log: Text("Execution Log")
        }
    }
}
