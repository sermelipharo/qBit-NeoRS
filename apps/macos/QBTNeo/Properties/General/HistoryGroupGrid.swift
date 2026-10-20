import SwiftUI

/// "History" group: the torrent's dates and creator (features.md §2.3.2–3).
/// Dates that are not set yet ("Completed On" while downloading) are dimmed.
struct HistoryGroupGrid: View {
    let values: TorrentProperties

    var body: some View {
        let v = values
        let metadata = v.hasMetadata
        let lastSeen = v.lastSeen > 0 ? ValueFormatter.date(v.lastSeen) : String(localized: "Never")
        PropertyGroup(title: "History") {
            GridRow { PropertyValueRow(label: "Added On:", value: ValueFormatter.date(v.additionDate)) }
            GridRow { PropertyValueRow(label: "Completed On:", value: ValueFormatter.date(v.completionDate)) }
            GridRow { PropertyValueRow(label: "Last Seen Complete:", value: lastSeen) }
            GridRow { PropertyValueRow(label: "Created On:", value: metadata ? ValueFormatter.date(v.creationDate) : "") }
            GridRow { PropertyValueRow(label: "Created By:", value: metadata ? v.createdBy : "") }
        }
    }
}
