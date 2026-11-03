import SwiftUI

/// Filter row and results table (AppKit, see `SearchResultsTable`) of one search tab.
struct SearchResultsView: View {
    let model: SearchModel
    let tab: SearchTabModel
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(spacing: 0) {
            SearchFilterBar(tab: tab)
            ListTableView(gui: app.gui) { SearchResultsTable.makeController(model: model, tab: tab, app: app) }
                .id(tab.id)
                .overlay {
                    if tab.rows.isEmpty, tab.status != .ongoing {
                        ContentUnavailableView(tab.status.text, systemImage: tab.status.symbol)
                    } else if !tab.rows.isEmpty, tab.visibleRows.isEmpty {
                        SearchFilteredOutView(tab: tab)
                    }
                }
        }
    }
}
