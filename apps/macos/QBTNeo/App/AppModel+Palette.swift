import AppKit

/// Navigation used by the Command Palette and the command registry: windows,
/// Settings, jumping to a torrent and applying a sidebar filter.
extension AppModel {
    func show(_ window: AppWindowID) {
        switch window {
        case .search: show(pane: .search)
        case .rss: show(pane: .rss)
        case .log: show(pane: .log)
        default: openWindow?(id: window.rawValue)
        }
    }

    /// Brings the main window forward on a pane, enabling Search or RSS if needed.
    func show(pane: MainPane) {
        switch pane {
        case .search: gui.searchEnabled = true
        case .rss: gui.rssEnabled = true
        case .transfers, .log: break
        }
        mainPane = pane
        if !isMainWindowVisible { openWindow?(id: AppWindowID.main.rawValue) }
        mainWindow?.makeKeyAndOrderFront(nil)
    }

    /// Searches the installed plugins for a phrase typed in the transfers filter field.
    func searchPlugins(for pattern: String) {
        let trimmed = pattern.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        pendingPluginSearch = trimmed
        show(pane: .search)
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
        case .rss: break
        }
        list.select(filter)
    }
}
