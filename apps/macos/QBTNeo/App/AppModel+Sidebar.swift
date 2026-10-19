import Foundation

extension AppModel {
    /// Hides or shows a whole sidebar section. A hidden section filters
    /// nothing: its filter goes back to "All", as when it is collapsed.
    func setSidebarSection(_ section: SidebarItem.Section, hidden: Bool) {
        if hidden {
            gui.hiddenSidebarSections.insert(section)
            switch section {
            case .status: list.statusFilter = .all
            case .category: list.categoryFilter = .all
            case .tag: list.tagFilter = .all
            case .trackerStatus: list.trackerStatusFilter = .all
            case .tracker: list.trackerFilter = .all
            case .rss: break
            }
        } else {
            gui.hiddenSidebarSections.remove(section)
        }
    }

    /// Unread RSS articles, shown on the sidebar's RSS row.
    var rssUnread: Int { Int(rssTab.totalUnread) }
}
