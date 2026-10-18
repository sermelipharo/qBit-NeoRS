import AppKit

/// Navigation used by the Command Palette and the command registry: windows,
/// Settings, jumping to a torrent and applying a sidebar filter.
extension AppModel {
    func show(_ window: AppWindowID) {
        openWindow?(id: window.rawValue)
    }

    func showSettings() {
        openSettings?()
    }

    /// Opens Settings on the setting's pane, revealing and highlighting it.
    func revealSetting(_ id: String) {
        pendingSettingReveal = id
        showSettings()
    }

    /// Selects the torrent and scrolls it into view, resetting the filters first if they hide it.
    func revealTorrent(_ id: String) {
        guard transfers.item(id) != nil else { return }
        if !list.visibleItems.contains(where: { $0.id == id }) { list.resetAllFilters() }
        list.selection = [id]
    }

    /// Applies a sidebar filter, showing the sidebar and expanding the filter's section.
    func applyFilter(_ filter: SidebarItem) {
        if !gui.filtersSidebarVisible { gui.filtersSidebarVisible = true }
        switch filter.section {
        case .status: gui.statusSectionExpanded = true
        case .category: gui.categorySectionExpanded = true
        case .tag: gui.tagSectionExpanded = true
        case .trackerStatus: gui.trackerStatusSectionExpanded = true
        case .tracker: gui.trackerSectionExpanded = true
        }
        list.select(filter)
    }
}
