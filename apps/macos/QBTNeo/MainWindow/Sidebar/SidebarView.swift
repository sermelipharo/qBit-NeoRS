import SwiftUI

/// The filters sidebar (an AppKit source list, see `SidebarController`).
/// Each section keeps its own selected row; all section filters combine
/// (AND) with each other and the search field.
struct SidebarView: View {
    @Environment(AppModel.self) private var model
    @Environment(GUIPreferences.self) private var gui

    var body: some View {
        SidebarOutline(model: model)
            .onChange(of: model.trackers.sortedHosts, initial: true) { model.requestTrackerFavicons() }
            .onChange(of: gui.downloadTrackerFavicon) { model.requestTrackerFavicons() }
    }
}
