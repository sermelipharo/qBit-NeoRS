import AppKit

/// The Trackers tab table (features.md §2.4): DHT/PeX/LSD rows first,
/// trackers with expandable announce endpoints, sortable columns, the
/// tracker context menu, Return/double-click to edit, Delete and Copy.
@MainActor
enum TrackerTable {
    static let specs: [TableColumnSpec] = [
        TableColumnSpec("url", String(localized: "URL/Announce Endpoint"), minWidth: 160, idealWidth: 320, isHideable: false),
        TableColumnSpec("tier", String(localized: "Tier"), minWidth: 30, idealWidth: 40, alignment: .right),
        TableColumnSpec("protocol", String(localized: "BT Protocol"), idealWidth: 70, isVisibleByDefault: false),
        TableColumnSpec("status", String(localized: "Status"), minWidth: 70, idealWidth: 110),
        TableColumnSpec("peers", String(localized: "Peers"), idealWidth: 60, alignment: .right),
        TableColumnSpec("seeds", String(localized: "Seeds"), idealWidth: 60, alignment: .right),
        TableColumnSpec("leeches", String(localized: "Leeches"), idealWidth: 60, alignment: .right),
        TableColumnSpec("downloaded", String(localized: "Times Downloaded"), minWidth: 50, idealWidth: 110, alignment: .right),
        TableColumnSpec("message", String(localized: "Message"), minWidth: 80, idealWidth: 240),
        TableColumnSpec("next", String(localized: "Next Announce"), minWidth: 50, idealWidth: 100, alignment: .right),
        TableColumnSpec("min", String(localized: "Min Announce"), minWidth: 50, idealWidth: 100, alignment: .right),
    ]

    static func makeController(model: AppModel) -> ListTableController<TrackerRow> {
        let properties = model.properties
        var configuration = ListTableController<TrackerRow>.Configuration(
            identifier: "TrackerList",
            accessibilityLabel: String(localized: "Trackers"),
            specs: specs,
            layoutKey: GUIPreferenceKey.trackerListColumns.rawValue,
            rows: { properties.trackerRows },
            selection: { properties.trackerSelection },
            setSelection: { properties.trackerSelection = $0 },
            content: { column, row in content(column, row, expanded: properties.expandedTrackers.contains(row.url)) }
        )
        configuration.sortOrder = { properties.trackerSortOrder }
        configuration.setSortOrder = { properties.trackerSortOrder = $0 }
        configuration.comparator = comparator
        configuration.makeCell = { table, column, row in
            guard column == "url" else { return nil }
            let cell = TrackerURLCell.make(in: table)
            cell.toggle = { properties.toggleTrackerExpansion($0) }
            cell.configure(row, expanded: properties.expandedTrackers.contains(row.url))
            return cell
        }
        configuration.configureCell = { view, _, row in
            (view as? TrackerURLCell)?.configure(row, expanded: properties.expandedTrackers.contains(row.url))
        }
        configuration.menu = { ids in
            guard let item = model.list.currentItem else { return nil }
            return TrackerContextMenu(model: model, item: item, ids: ids).makeMenu()
        }
        configuration.primaryAction = { ids in
            guard let item = model.list.currentItem else { return }
            model.requestEditTracker(hash: item.id, rowIDs: ids)
        }
        configuration.deleteAction = {
            guard let item = model.list.currentItem else { return }
            model.removeSelectedTrackers(hash: item.id)
        }
        configuration.copyAction = {
            let urls = model.selectedTrackerURLs()
            if !urls.isEmpty { model.copyToPasteboard(urls.joined(separator: "\n")) }
        }
        configuration.pasteAction = { model.pasteTrackers() }
        let controller = ListTableController(configuration)
        // Expanding a tracker blanks its totals and turns its chevron.
        controller.observe({ properties.expandedTrackers }, apply: { [weak controller] _ in controller?.refreshAll() })
        return controller
    }

    private static func content(_ column: String, _ row: TrackerRow, expanded: Bool) -> TableCellContent {
        let alignment = specs.first { $0.id == column }?.alignment ?? .natural
        switch column {
        case "tier":
            return TableCellContent(row.kind == .tracker ? row.tier.formatted() : "", monospacedDigits: true, alignment: alignment)
        case "protocol":
            return TableCellContent(row.btVersion > 0 ? "v\(row.btVersion)" : "")
        default:
            // Aggregated values are blanked on expanded trackers (their endpoint rows show them).
            let text = row.kind == .tracker && expanded ? "" : Self.text(column, row)
            let color: NSColor? = switch (row.kind, row.status) {
            case (.sticky, _): .secondaryLabelColor
            case (_, .trackerError), (_, .notWorking), (_, .unreachable): TorrentState.error.nsColor
            default: nil
            }
            let longText = column == "status" || column == "message"
            return TableCellContent(text, color: color, monospacedDigits: true, alignment: alignment,
                                    toolTip: longText && !text.isEmpty ? text : nil)
        }
    }

    private static func text(_ column: String, _ row: TrackerRow) -> String {
        switch column {
        case "status": row.statusText
        case "peers": ValueFormatter.count(row.peers)
        case "seeds": ValueFormatter.count(row.seeds)
        case "leeches": ValueFormatter.count(row.leeches)
        case "downloaded": ValueFormatter.count(row.downloaded)
        case "message": row.message
        case "next": DurationFormatter.string(max(row.nextAnnounce, 0), resolution: .seconds)
        case "min": DurationFormatter.string(max(row.minAnnounce, 0), resolution: .seconds)
        default: ""
        }
    }

    private static func comparator(_ key: String, _ order: SortOrder) -> KeyPathComparator<TrackerRow>? {
        switch key {
        case "url": KeyPathComparator(\TrackerRow.url, order: order)
        case "tier": KeyPathComparator(\TrackerRow.tier, order: order)
        case "protocol": KeyPathComparator(\TrackerRow.btVersion, order: order)
        case "status": KeyPathComparator(\TrackerRow.statusText, order: order)
        case "peers": KeyPathComparator(\TrackerRow.peers, order: order)
        case "seeds": KeyPathComparator(\TrackerRow.seeds, order: order)
        case "leeches": KeyPathComparator(\TrackerRow.leeches, order: order)
        case "downloaded": KeyPathComparator(\TrackerRow.downloaded, order: order)
        case "message": KeyPathComparator(\TrackerRow.message, order: order)
        case "next": KeyPathComparator(\TrackerRow.nextAnnounce, order: order)
        case "min": KeyPathComparator(\TrackerRow.minAnnounce, order: order)
        default: nil
        }
    }
}
