import AppKit

/// Links from the widgets (`qbitneors://torrent/<hash>`): clicking a Torrent
/// widget opens the main window with that torrent selected, through the same
/// navigation the Command Palette uses (`revealTorrent`).
extension AppModel {
    /// Handles a widget link; returns false for other URLs (magnet links, files).
    func openWidgetLink(_ url: URL) -> Bool {
        guard url.scheme?.lowercased() == WidgetLink.scheme else { return false }
        guard let id = WidgetLink.torrentID(from: url) else { return true }
        openWindow?(id: AppWindowID.main.rawValue)
        NSApp.activate()
        if transfers.item(id) != nil {
            revealTorrent(id)
        } else {
            // Launched by the click: select it once the transfer list arrives.
            pendingRevealID = id
        }
        return true
    }

    /// Called after a transfer-list sync.
    func revealPendingTorrent() {
        guard let id = pendingRevealID else { return }
        pendingRevealID = nil
        revealTorrent(id)
    }
}
