import SwiftUI

/// General tab (features.md §2.3): pieces/availability bars, Transfer and
/// Information groups.
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
                PiecesSection(item: item, values: values, pieces: properties.pieces, availability: properties.pieceAvailability)
                GroupBox("Transfer") {
                    TransferGroupGrid(values: values, isFinished: item.isFinished)
                        .padding(Design.Spacing.small)
                }
                GroupBox("Information") {
                    InformationGroupGrid(values: values)
                        .padding(Design.Spacing.small)
                }
            }
            .padding(Design.Spacing.medium)
        }
        .font(.callout)
        .environment(\.propertiesValuesSelectable, valuesSelectable)
        .onHover { inside in
            if inside, !valuesSelectable { valuesSelectable = true }
        }
    }
}
