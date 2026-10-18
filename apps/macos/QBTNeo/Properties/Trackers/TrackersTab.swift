import SwiftUI

/// Trackers tab (features.md §2.4): the tracker table (AppKit, see
/// `TrackerTable`) and the tier buttons beside it.
struct TrackersTab: View {
    let item: TorrentItem
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(spacing: 0) {
            ListTableView(gui: model.gui) { TrackerTable.makeController(model: model) }
                .overlay { PropertiesFailureOverlay(tab: .trackers, isEmpty: model.properties.trackerRows.isEmpty) }
            Divider()
            TrackerTierButtons(item: item)
        }
    }
}
