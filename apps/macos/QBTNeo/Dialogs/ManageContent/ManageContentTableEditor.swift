import AppKit

/// Starts editing the Path cell of the clicked or selected row (double-click
/// or Return in the Manage Torrent Content table).
@MainActor
final class ManageContentTableEditor {
    weak var table: NSTableView?

    func beginEditing() {
        guard let table,
              let column = table.tableColumns.firstIndex(where: { $0.identifier.rawValue == ManageContentTable.pathColumn })
        else { return }
        let row = table.clickedRow >= 0 ? table.clickedRow : table.selectedRow
        guard row >= 0 else { return }
        table.scrollRowToVisible(row)
        table.editColumn(column, row: row, with: nil, select: true)
    }
}
