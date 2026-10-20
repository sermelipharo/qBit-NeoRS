import SwiftUI

/// General tab (features.md §2.3): a summary strip (state, speeds, ETA,
/// ratio, seeds, peers), the pieces/availability bars, then the Transfer,
/// Connection, History and Information groups in as many columns as fit,
/// and the comment.
struct GeneralTab: View {
    let item: TorrentItem
    @Environment(AppModel.self) private var model
    /// Set once the pointer enters the tab; values stay selectable from then on.
    @State private var valuesSelectable = false

    var body: some View {
        let properties = model.properties
        let values = properties.general ?? TorrentProperties(item: item)
        ScrollView {
            VStack(alignment: .leading, spacing: Design.Spacing.medium) {
                GeneralSummaryStrip(item: item, values: values)
                PiecesSection(item: item, values: values, pieces: properties.pieces, availability: properties.pieceAvailability)
                Divider()
                AdaptiveColumnsLayout(horizontalSpacing: Design.Spacing.xLarge, verticalSpacing: Design.Spacing.medium, maxIdealWidth: 300) {
                    TransferGroupGrid(values: values, isFinished: item.isFinished)
                    ConnectionGroupGrid(values: values)
                    HistoryGroupGrid(values: values)
                    InformationGroupGrid(id: item.id, values: values)
                }
                if !values.comment.isEmpty {
                    GeneralCommentRow(comment: values.comment)
                }
            }
            .padding(.horizontal, Design.Spacing.large)
            .padding(.vertical, Design.Spacing.medium)
        }
        .font(.callout)
        .environment(\.propertiesValuesSelectable, valuesSelectable)
        .onHover { inside in
            if inside, !valuesSelectable { valuesSelectable = true }
        }
    }
}
