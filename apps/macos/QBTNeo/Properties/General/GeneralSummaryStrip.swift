import SwiftUI

/// The top of the General tab: state, speeds with their averages, ETA,
/// ratio, seeds and peers, wrapping into more rows on narrow panels.
struct GeneralSummaryStrip: View {
    let item: TorrentItem
    let values: TorrentProperties

    var body: some View {
        let v = values
        let speed = UnitFormatter.speed
        AdaptiveColumnsLayout(horizontalSpacing: Design.Spacing.large, verticalSpacing: Design.Spacing.regular, maxIdealWidth: 240) {
            GeneralMetric(title: "Status", symbol: item.state.systemImage, tint: item.state.color, value: item.statusText,
                          caption: statusCaption, valueStyle: Color(nsColor: item.state.cardTextNSColor))
            GeneralMetric(title: "Down Speed", symbol: Symbol.arrowDown, tint: Design.Palette.download, value: speed(v.dlSpeed),
                          caption: Self.average(v.dlSpeedAvg))
            GeneralMetric(title: "Up Speed", symbol: Symbol.arrowUp, tint: Design.Palette.upload, value: speed(v.upSpeed),
                          caption: Self.average(v.upSpeedAvg))
            GeneralMetric(title: "ETA", symbol: Symbol.clock, tint: .secondary, value: DurationFormatter.eta(v.eta),
                          caption: String(localized: "ETA"))
            GeneralMetric(title: "Ratio", symbol: Symbol.arrowUpArrowDown, tint: .secondary, value: ValueFormatter.ratio(v.shareRatio),
                          caption: String(localized: "Ratio"))
            GeneralMetric(title: "Seeds", symbol: Symbol.leaf, tint: .secondary, value: ValueFormatter.connectedOfTotal(v.seeds, v.seedsTotal),
                          caption: String(localized: "Seeds"))
            GeneralMetric(title: "Peers", symbol: Symbol.person2, tint: .secondary, value: ValueFormatter.connectedOfTotal(v.peers, v.peersTotal),
                          caption: String(localized: "Peers"))
        }
    }

    /// The error message of a failed torrent, otherwise how much of it is here.
    private var statusCaption: String {
        if item.state.isErrored, !item.errorMessage.isEmpty { return item.errorMessage }
        if item.progress >= 1 || item.completed >= item.size { return UnitFormatter.size(item.size) }
        return String(localized: "cards.sizeOf", defaultValue: "\(UnitFormatter.size(item.completed)) of \(UnitFormatter.size(item.size))",
                      comment: "Card: downloaded of total size")
    }

    private static func average(_ speed: Int64) -> String {
        String(localized: "general.averageSpeed", defaultValue: "avg. \(UnitFormatter.speed(speed))",
               comment: "General tab: average speed below the current one")
    }
}
