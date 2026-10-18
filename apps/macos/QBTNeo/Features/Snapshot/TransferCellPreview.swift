import AppKit
import SwiftUI

/// One transfer-list cell outside the table (snapshot of the Category and
/// Tags cells).
struct TransferCellPreview: NSViewRepresentable {
    let item: TorrentItem
    let column: TransferColumn
    let model: AppModel

    func makeNSView(context: Context) -> TableTextCell {
        let cell = TableTextCell(identifier: NSUserInterfaceItemIdentifier("preview"))
        cell.configure(TransferCell(gui: model.gui, labels: model.labels, names: model.names).content(column, item))
        return cell
    }

    func updateNSView(_ nsView: TableTextCell, context: Context) {
        nsView.configure(TransferCell(gui: model.gui, labels: model.labels, names: model.names).content(column, item))
    }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: TableTextCell, context: Context) -> CGSize? {
        CGSize(width: proposal.width ?? 240, height: 24)
    }
}
