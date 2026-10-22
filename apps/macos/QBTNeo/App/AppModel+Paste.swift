import AppKit

/// ⌘V where it is pressed: torrents in the transfer list, trackers in the
/// Trackers tab, peers in the Peers tab (`PastedText` sorts the lines).
extension AppModel {
    /// The transfer list: copied .torrent files or torrent links are added;
    /// text with only trackers or only peers goes to the selected torrents.
    func pasteFromClipboard() {
        let pasteboard = NSPasteboard.general
        if let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL],
           !urls.isEmpty {
            addTorrentFiles(urls.filter { $0.pathExtension.lowercased() == "torrent" })
            return
        }
        guard let text = pasteboard.string(forType: .string) else { return NSSound.beep() }
        let pasted = PastedText(text, context: .transfers)
        let links = pasted.torrentLinks.filter(Self.isTorrentLink)
        let selected = list.selectedItems
        if !links.isEmpty {
            addTorrentLinks(fromText: links.joined(separator: "\n"))
        } else if pasted.trackerCount > 0, !selected.isEmpty {
            let entries = TrackerTierParser.parse(pasted.trackerText)
            perform { client in
                for item in selected { try client.addTrackers(hash: item.id, trackers: entries) }
            }
        } else if !pasted.peers.isEmpty, !selected.isEmpty {
            let hashes = selected.filter { addPeersDisabledReason($0) == nil }.map(\.id)
            guard !hashes.isEmpty else { return NSSound.beep() }
            perform { try $0.addPeers(hashes: hashes, peers: pasted.peers) }
        } else {
            NSSound.beep()
        }
    }

    /// The Trackers tab: tracker URLs (and a magnet's trackers) join the
    /// torrent in new tiers after its last one, like "Add trackers".
    func pasteTrackers() {
        guard let item = list.currentItem, let text = NSPasteboard.general.string(forType: .string) else { return NSSound.beep() }
        let pasted = PastedText(text, context: .trackers)
        guard pasted.trackerCount > 0 else { return NSSound.beep() }
        let lastTier = properties.trackerEntries.filter { $0.tier >= 0 }.map(\.tier).max()
        let entries = TrackerTierParser.parse(pasted.trackerText, baseTier: lastTier.map { $0 + 1 } ?? 0)
        perform { try $0.addTrackers(hash: item.id, trackers: entries) }
    }

    /// The Peers tab: `IP:port` lines (and a magnet's peers) are added, like "Add Peers…".
    func pastePeers() {
        guard let item = list.currentItem, let text = NSPasteboard.general.string(forType: .string),
              addPeersDisabledReason(item) == nil else { return NSSound.beep() }
        let peers = PastedText(text, context: .transfers).peers
        guard !peers.isEmpty else { return NSSound.beep() }
        perform { try $0.addPeers(hashes: [item.id], peers: peers) }
    }
}
