import SwiftUI

/// "Transfer" group: activity and amounts (features.md §2.3.2). Speeds,
/// ETA, ratio, seeds and peers are in `GeneralSummaryStrip`; connections
/// and limits in `ConnectionGroupGrid`; Last Seen Complete in `HistoryGroupGrid`.
struct TransferGroupGrid: View {
    let values: TorrentProperties
    let isFinished: Bool

    var body: some View {
        let v = values
        let size = UnitFormatter.size
        PropertyGroup(title: "Transfer") {
            GridRow { PropertyValueRow(label: "Time Active:", value: timeActive) }
            GridRow {
                PropertyValueRow(label: "Downloaded:", value: String(localized: "\(size(v.totalDownloaded)) (\(size(v.totalDownloadedSession)) this session)"))
            }
            GridRow {
                PropertyValueRow(label: "Uploaded:", value: String(localized: "\(size(v.totalUploaded)) (\(size(v.totalUploadedSession)) this session)"))
            }
            GridRow { PropertyValueRow(label: "Wasted:", value: size(v.totalWasted)) }
            GridRow {
                PropertyValueRow(label: "Popularity:", value: ValueFormatter.ratio(v.popularity),
                                 help: "Ratio / Time Active (in months), indicates how popular the torrent is")
            }
        }
    }

    private var timeActive: String {
        isFinished
            ? String(localized: "\(DurationFormatter.string(values.timeElapsed)) (seeded for \(DurationFormatter.string(values.seedingTime)))")
            : DurationFormatter.string(values.timeElapsed)
    }
}
