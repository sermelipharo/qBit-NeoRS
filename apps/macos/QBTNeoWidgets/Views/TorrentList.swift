import SwiftUI

/// The torrents matching the widget's filter, or why there are none.
struct TorrentList: View {
    let entry: SnapshotEntry
    let snapshot: WidgetSnapshot
    let limit: Int
    var spacing = 8.0

    var body: some View {
        if snapshot.status == .locked {
            WidgetNotice(content: WidgetNoticeContent(symbol: "lock.fill", title: String(localized: "qBit NeoRS is locked"), message: ""),
                         compact: true)
        } else {
            let torrents = snapshot.torrents(filter: entry.filter, category: entry.category, limit: limit)
            if torrents.isEmpty {
                WidgetNotice(content: WidgetNoticeContent(symbol: entry.filter.emptySymbol, title: entry.filter.emptyMessage, message: ""),
                             compact: true)
            } else {
                VStack(alignment: .leading, spacing: spacing) {
                    ForEach(torrents) { TorrentRow(torrent: $0) }
                }
                .frame(maxHeight: .infinity, alignment: .top)
            }
        }
    }
}
