import Foundation

/// Sort By: the transfer list's main sort field and direction, shared by the
/// toolbar, View › Sort By, the Command Palette and the list's column headers.
extension CommandRegistry {
    static var sortCommands: [AppCommand] {
        let sortWords = words(String(localized: "palette.keywords.sort", defaultValue: "order, arrange",
                                     comment: "Command Palette search words, comma-separated"))
        let fields = TransferColumn.sortFields.compactMap { column -> AppCommand? in
            guard let id = column.sortCommand else { return nil }
            let command = AppCommand(id, String(localized: "Sort by \(column.sortTitle)", comment: "Command Palette: sort torrents by a field"),
                                     symbol: Symbol.arrowUpArrowDown, keywords: sortWords) {
                $0.model.list.sortColumn = column
            }
            .state { $0.list.sortColumn == column }
            return column == .queuePosition ? command.available { $0.model.queueingEnabled } : command
        }
        return fields + [
            AppCommand(.sortAscending, String(localized: "sort.command.ascending", defaultValue: "Sort Ascending",
                                              comment: "Command Palette: sort torrents in ascending order"),
                       symbol: Symbol.arrowUp, keywords: sortWords) { $0.model.list.sortAscending = true }
                .state { $0.list.sortAscending },
            AppCommand(.sortDescending, String(localized: "sort.command.descending", defaultValue: "Sort Descending",
                                               comment: "Command Palette: sort torrents in descending order"),
                       symbol: Symbol.arrowDown, keywords: sortWords) { $0.model.list.sortAscending = false }
                .state { !$0.list.sortAscending },
        ]
    }
}
