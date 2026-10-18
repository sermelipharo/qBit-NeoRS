import SwiftUI

/// Peers tab (features.md §2.5): the peer table (AppKit, see `PeerTable`).
struct PeersTab: View {
    let item: TorrentItem
    @Environment(AppModel.self) private var model

    var body: some View {
        ListTableView(gui: model.gui) { PeerTable.makeController(model: model) }
            .overlay { PropertiesFailureOverlay(tab: .peers, isEmpty: model.properties.peers.isEmpty) }
    }
}
