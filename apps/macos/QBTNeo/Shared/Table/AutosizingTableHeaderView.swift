import AppKit

/// The column header of `NativeTableView`: a double-click on a column's
/// right divider fits that column to its content, an ⌥-click on any divider
/// fits all of them (like Finder's list view).
@MainActor
final class AutosizingTableHeaderView: NSTableHeaderView {
    private static let dividerSlop: CGFloat = 4

    override func mouseDown(with event: NSEvent) {
        guard let table = tableView as? NativeTableView, let column = dividerColumn(at: convert(event.locationInWindow, from: nil)) else {
            super.mouseDown(with: event)
            return
        }
        if event.modifierFlags.contains(.option) {
            table.sizeAllColumnsToFit()
        } else if event.clickCount == 2 {
            table.sizeColumnToFit(column)
        } else {
            super.mouseDown(with: event)
        }
    }

    /// The column whose right edge is under the point.
    private func dividerColumn(at point: NSPoint) -> NSTableColumn? {
        guard let table = tableView else { return nil }
        for (index, column) in table.tableColumns.enumerated() where !column.isHidden {
            let edge = table.rect(ofColumn: index).maxX
            if abs(point.x - edge) <= Self.dividerSlop { return column }
        }
        return nil
    }
}
