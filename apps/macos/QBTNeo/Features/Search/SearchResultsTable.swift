import AppKit

/// The results table of one search tab: sortable columns, visited rows in
/// grey, double-click/Return opens the download window, and the result
/// context menu (features.md §5.8.1).
@MainActor
enum SearchResultsTable {
    static let specs: [TableColumnSpec] = [
        TableColumnSpec("name", String(localized: "Name"), minWidth: 200, idealWidth: 420, isHideable: false),
        TableColumnSpec("size", String(localized: "Size"), minWidth: 60, idealWidth: 80, alignment: .right),
        TableColumnSpec("seeders", String(localized: "Seeders"), minWidth: 50, idealWidth: 60, alignment: .right),
        TableColumnSpec("leechers", String(localized: "Leechers"), minWidth: 50, idealWidth: 60, alignment: .right),
        TableColumnSpec("engine", String(localized: "Engine"), minWidth: 70, idealWidth: 110),
        TableColumnSpec("engineURL", String(localized: "Engine URL"), minWidth: 80, idealWidth: 160),
        TableColumnSpec("published", String(localized: "Published On"), minWidth: 90, idealWidth: 130),
    ]

    static func makeController(model: SearchModel, tab: SearchTabModel, app: AppModel) -> ListTableController<SearchResultRow> {
        var configuration = ListTableController<SearchResultRow>.Configuration(
            identifier: "SearchResults",
            accessibilityLabel: String(localized: "Search results"),
            specs: specs,
            layoutKey: GUIPreferenceKey.searchResultColumns.rawValue,
            rows: { tab.visibleRows },
            selection: { tab.selection },
            setSelection: { tab.selection = $0 },
            content: { column, row in content(column, row, visited: tab.visited.contains(row.id)) }
        )
        configuration.sortOrder = { tab.sortOrder }
        configuration.setSortOrder = { tab.sortOrder = $0 }
        configuration.comparator = comparator
        configuration.menu = { ids in
            let rows = tab.rows.filter { ids.contains($0.id) }
            guard !rows.isEmpty else { return nil }
            return menu(model: model, tab: tab, app: app, rows: rows)
        }
        configuration.primaryAction = { ids in
            model.download(tab.rows.filter { ids.contains($0.id) }, in: tab, app: app, skipDialog: false)
        }
        let controller = ListTableController(configuration)
        controller.observe({ tab.visited }, apply: { [weak controller] _ in controller?.refreshAll() })
        return controller
    }

    private static func content(_ column: String, _ row: SearchResultRow, visited: Bool) -> TableCellContent {
        switch column {
        case "name": TableCellContent(row.name, color: visited ? .secondaryLabelColor : nil, toolTip: row.name)
        case "size": TableCellContent(row.sizeText, monospacedDigits: true, alignment: .right)
        case "seeders": TableCellContent(row.seeders.formatted(), monospacedDigits: true, alignment: .right)
        case "leechers": TableCellContent(row.leechers.formatted(), monospacedDigits: true, alignment: .right)
        case "engine": TableCellContent(row.engine)
        case "engineURL": TableCellContent(row.engineURL, color: .secondaryLabelColor)
        case "published": TableCellContent(row.publishedText, monospacedDigits: true)
        default: TableCellContent()
        }
    }

    private static func comparator(_ key: String, _ order: SortOrder) -> KeyPathComparator<SearchResultRow>? {
        switch key {
        case "name": KeyPathComparator(\SearchResultRow.name, order: order)
        case "size": KeyPathComparator(\SearchResultRow.size, order: order)
        case "seeders": KeyPathComparator(\SearchResultRow.seeders, order: order)
        case "leechers": KeyPathComparator(\SearchResultRow.leechers, order: order)
        case "engine": KeyPathComparator(\SearchResultRow.engine, order: order)
        case "engineURL": KeyPathComparator(\SearchResultRow.engineURL, order: order)
        case "published": KeyPathComparator(\SearchResultRow.published, order: order)
        default: nil
        }
    }

    private static func menu(model: SearchModel, tab: SearchTabModel, app: AppModel, rows: [SearchResultRow]) -> NSMenu {
        let menu = NSMenu.manual()
        menu.addAction(String(localized: "Open download window"), symbol: Symbol.arrowDownDoc) {
            model.download(rows, in: tab, app: app, skipDialog: false)
        }
        menu.addAction(String(localized: "Download"), symbol: Symbol.arrowDownCircle) {
            model.download(rows, in: tab, app: app, skipDialog: true)
        }
        menu.addDivider()
        menu.addAction(String(localized: "Open description page"), symbol: Symbol.safari) { model.openDescriptions(rows, app: app) }
        menu.addSubmenu(String(localized: "Copy")) { copy in
            copy.addAction(String(localized: "Name")) { model.copy(rows, \.fileName) }
            copy.addAction(String(localized: "Download link")) { model.copy(rows, \.fileUrl) }
            copy.addAction(String(localized: "Description page URL")) { model.copy(rows, \.descrLink) }
        }
        return menu
    }
}
