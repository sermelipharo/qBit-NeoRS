import SwiftUI

/// "Connection" group: connections, speed limits and the next reannounce
/// (features.md §2.3.2).
struct ConnectionGroupGrid: View {
    let values: TorrentProperties

    var body: some View {
        let v = values
        let maxConnections = v.connectionsLimit < 0 ? UnitFormatter.infinity : v.connectionsLimit.formatted()
        PropertyGroup(title: "Connection") {
            GridRow { PropertyValueRow(label: "Connections:", value: String(localized: "\(v.connections) (\(maxConnections) max)")) }
            GridRow { PropertyValueRow(label: "Download Limit:", value: UnitFormatter.limit(v.dlLimit)) }
            GridRow { PropertyValueRow(label: "Upload Limit:", value: UnitFormatter.limit(v.upLimit)) }
            GridRow { PropertyValueRow(label: "Reannounce In:", value: DurationFormatter.string(v.reannounce)) }
        }
    }
}
