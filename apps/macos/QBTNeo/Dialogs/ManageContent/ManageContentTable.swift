import AppKit

/// The file table of the Manage Torrent Content dialog: Index and Path
/// (editable), in file order, multiple selection; an AppKit table so a
/// torrent with thousands of files stays light.
@MainActor
enum ManageContentTable {
    static let indexColumn = "index"
    static let pathColumn = "path"

    static var specs: [TableColumnSpec] {
        [
            TableColumnSpec(indexColumn, String(localized: "Index"), minWidth: 36, idealWidth: 52, alignment: .right, isSortable: false),
            TableColumnSpec(pathColumn, String(localized: "Path"), minWidth: 160, idealWidth: 560, isHideable: false, isSortable: false),
        ]
    }

    static func makeController(dialog: ManageContentModel) -> ListTableController<ManageContentRow> {
        let editor = ManageContentTableEditor()
        var configuration = ListTableController<ManageContentRow>.Configuration(
            identifier: "ManageContent",
            accessibilityLabel: String(localized: "Manage Torrent Content"),
            specs: specs,
            layoutKey: GUIPreferenceKey.manageContentColumns.rawValue,
            rows: { dialog.rows },
            selection: { dialog.selection },
            setSelection: { dialog.select($0) },
            content: { _, row in TableCellContent(String(row.id), monospacedDigits: true, alignment: .right) }
        )
        configuration.makeCell = { table, column, row in
            guard column == pathColumn else { return nil }
            let cell = ManageContentPathCell.make(in: table)
            cell.onEdit = { file, text in dialog.setPath(text, forFile: file) }
            cell.configure(row)
            return cell
        }
        configuration.configureCell = { view, _, row in
            (view as? ManageContentPathCell)?.configure(row)
        }
        configuration.primaryAction = { _ in editor.beginEditing() }
        let controller = ListTableController(configuration)
        editor.table = controller.table
        return controller
    }
}
