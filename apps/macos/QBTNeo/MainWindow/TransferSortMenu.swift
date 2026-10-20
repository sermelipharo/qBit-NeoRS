import SwiftUI

/// The main toolbar's Sort By pull-down, like Finder's and Mail's: the sort
/// field, then Ascending / Descending. It edits the list model's sort order,
/// the same state as the list's column headers, View › Sort By and the
/// Command Palette, and it stays in the toolbar in both the list and the
/// cards so the List / Cards switch next to it never moves.
struct TransferSortMenu: View {
    @Bindable var list: TransferListModel
    let queueingEnabled: Bool

    static var title: String {
        String(localized: "sort.menu", defaultValue: "Sort By", comment: "Toolbar and View menu: choose the transfer list's sort order")
    }

    static var ascendingTitle: String {
        String(localized: "sort.ascending", defaultValue: "Ascending", comment: "Sort By: smallest, earliest or A first")
    }

    static var descendingTitle: String {
        String(localized: "sort.descending", defaultValue: "Descending", comment: "Sort By: largest, latest or Z first")
    }

    var body: some View {
        Menu {
            Picker(Self.title, selection: $list.sortColumn) {
                ForEach(TransferColumn.sortFields(queueing: queueingEnabled, current: list.sortColumn), id: \.self) { column in
                    Text(column.sortTitle).tag(column)
                }
            }
            .pickerStyle(.inline)
            Picker(String(localized: "sort.direction", defaultValue: "Order", comment: "Sort By: the ascending / descending choice"),
                   selection: $list.sortAscending) {
                Text(Self.ascendingTitle).tag(true)
                Text(Self.descendingTitle).tag(false)
            }
            .pickerStyle(.inline)
        } label: {
            Label(Self.title, systemImage: Symbol.arrowUpArrowDown)
        }
        .help(Self.title)
        .accessibilityValue(Self.stateDescription(column: list.sortColumn, ascending: list.sortAscending))
    }

    /// "Name, Ascending": what VoiceOver reads after the button's name.
    static func stateDescription(column: TransferColumn, ascending: Bool) -> String {
        "\(column.sortTitle), \(ascending ? ascendingTitle : descendingTitle)"
    }
}
