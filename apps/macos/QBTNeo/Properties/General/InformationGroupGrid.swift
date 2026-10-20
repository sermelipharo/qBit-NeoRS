import SwiftUI

/// "Information" group (features.md §2.3.3): size, pieces, privacy, save
/// path (with Show in Finder) and info hashes (with Copy). Long values
/// truncate in the middle; the comment is `GeneralCommentRow`.
struct InformationGroupGrid: View {
    let id: String
    let values: TorrentProperties

    var body: some View {
        let v = values
        let metadata = v.hasMetadata
        PropertyGroup(title: "Information") {
            GridRow { PropertyValueRow(label: "Total Size:", value: metadata ? UnitFormatter.size(v.totalSize) : "") }
            GridRow {
                PropertyValueRow(label: "Pieces:", value: metadata
                    ? String(localized: "\(v.piecesNum) x \(UnitFormatter.size(v.pieceSize)) (have \(v.piecesHave))") : "")
            }
            GridRow {
                PropertyValueRow(label: "Private:", value: metadata ? ValueFormatter.yesNo(v.private) : String(localized: "N/A"),
                                 isPlaceholder: !metadata)
            }
            GridRow { PropertyValueRow(label: "Save Path:", value: v.savePath, action: .reveal(id: id)) }
            GridRow {
                PropertyValueRow(label: "Info Hash v1:", value: ValueFormatter.hash(v.infohashV1), isPlaceholder: v.infohashV1.isEmpty, action: .copy)
            }
            GridRow {
                PropertyValueRow(label: "Info Hash v2:", value: ValueFormatter.hash(v.infohashV2), isPlaceholder: v.infohashV2.isEmpty, action: .copy)
            }
        }
    }
}
