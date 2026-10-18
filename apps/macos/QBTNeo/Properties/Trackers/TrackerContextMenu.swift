import AppKit

/// Trackers tab context menu (features.md §2.4.4).
@MainActor
struct TrackerContextMenu {
    let model: AppModel
    let item: TorrentItem
    let ids: Set<String>

    func makeMenu() -> NSMenu {
        let menu = NSMenu.manual()
        let model = model
        let id = item.id
        let ids = ids
        let urls = model.trackerURLs(rowIDs: ids)
        let running = !item.state.isStopped
        menu.addAction(String(localized: "Add Trackers…"), symbol: Symbol.plus) { model.sheet = .addTrackers(id: id) }
        if !urls.isEmpty {
            menu.addAction(String(localized: "Edit Tracker URL…"), symbol: Symbol.pencil) { model.requestEditTracker(hash: id, rowIDs: ids) }
            menu.addAction(String(localized: "Remove Tracker"), symbol: Symbol.minusCircle) {
                model.perform { try $0.removeTrackers(hash: id, urls: urls) }
            }
            menu.addAction(String(localized: "Copy Tracker URL"), symbol: Symbol.docOnDoc) {
                model.copyToPasteboard(urls.joined(separator: "\n"))
            }
            if running {
                menu.addAction(String(localized: "Force Reannounce to Selected Trackers"), symbol: Symbol.arrowClockwise) {
                    model.reannounce([id])
                }
            }
        }
        if running {
            menu.addDivider()
            menu.addAction(String(localized: "Force Reannounce to All Trackers"), symbol: Symbol.arrowClockwiseCircle) {
                model.reannounce([id])
            }
        }
        return menu
    }
}
