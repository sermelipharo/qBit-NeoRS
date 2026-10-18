import SwiftUI

/// Snapshot entries for the cards view (`--snapshot-only cards`): a wide
/// grid with a selected card, the narrowest cards (two 240 pt columns), and
/// "Color by state: None". Each entry also prints the cards' heights.
@MainActor
enum CardSnapshots {
    private static let wide = CGSize(width: 1_000, height: 760)
    /// Two columns of the minimum card width.
    private static let narrow = CGSize(width: 2 * TransferCardMetrics.minWidth + 3 * TransferCardMetrics.spacing, height: 900)

    static var entries: [FeatureSnapshotCatalog.Entry] {
        [
            entry("cards", size: wide, scope: .wholeRow, selectsFirst: true),
            entry("cards-narrow", size: narrow, scope: .wholeRow, selectsFirst: false),
            entry("cards-nocolor", size: wide, scope: .off, selectsFirst: false),
        ]
    }

    private static func entry(_ name: String, size: CGSize, scope: StateColorScope, selectsFirst: Bool) -> FeatureSnapshotCatalog.Entry {
        FeatureSnapshotCatalog.Entry(name: name, title: AppWindowID.main.title, size: size) { model in
            model.gui.stateColorScope = scope
            model.list.selection = selectsFirst ? Set(model.list.visibleItems.prefix(1).map(\.id)) : []
            logHeights(model, name: name, width: size.width)
            return AnyView(TransferCardsView(model: model))
        }
    }

    /// Prints each card's own height and its grid row's height for a view `width` wide.
    private static func logHeights(_ model: AppModel, name: String, width: CGFloat) {
        let builder = TransferCardBuilder(gui: model.gui, labels: model.labels, names: model.names)
        let items = model.list.visibleItems
        let columns = TransferCardsLayout.columns(for: width)
        let rows = TransferCardHeights()
        let ids = items.map(\.id)
        for (index, item) in items.enumerated() {
            let own = TransferCardHeights()
            let height = own.rowHeight(at: 0, ids: [item.id], columns: 1, cardWidth: columns.itemWidth) { _ in builder.layoutInput(item) }
            let row = rows.rowHeight(at: index, ids: ids, columns: columns.count, cardWidth: columns.itemWidth) { builder.layoutInput(items[$0]) }
            print("snapshot \(name): card \(index) \(Int(columns.itemWidth))x\(Int(height)) row \(Int(row)) \(item.state) \(item.name)")
        }
    }
}
