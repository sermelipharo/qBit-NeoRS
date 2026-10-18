import AppKit

/// The Peers tab table (features.md §2.5): every peer column, sorting,
/// the peer context menu, Copy (IP:port) and Delete (ban).
@MainActor
enum PeerTable {
    static func specs(showsCountry: Bool) -> [TableColumnSpec] {
        [
            TableColumnSpec("country", String(localized: "Country/Region"), minWidth: 30, idealWidth: 40, isVisibleByDefault: showsCountry),
            TableColumnSpec("ip", String(localized: "IP/Address"), minWidth: 80, idealWidth: 150, isHideable: false),
            TableColumnSpec("port", String(localized: "Port"), minWidth: 36, idealWidth: 52, alignment: .right),
            TableColumnSpec("connection", String(localized: "Connection"), idealWidth: 70),
            TableColumnSpec("flags", String(localized: "Flags"), idealWidth: 80),
            TableColumnSpec("client", String(localized: "Client"), minWidth: 60, idealWidth: 150),
            TableColumnSpec("peerID", String(localized: "Peer ID Client"), minWidth: 60, idealWidth: 90, isVisibleByDefault: false),
            TableColumnSpec("progress", String(localized: "Progress"), minWidth: 44, idealWidth: 64, alignment: .right),
            TableColumnSpec("down", String(localized: "Down Speed"), minWidth: 50, idealWidth: 86, alignment: .right),
            TableColumnSpec("up", String(localized: "Up Speed"), minWidth: 50, idealWidth: 86, alignment: .right),
            TableColumnSpec("downloaded", String(localized: "Downloaded"), minWidth: 50, idealWidth: 86, alignment: .right),
            TableColumnSpec("uploaded", String(localized: "Uploaded"), minWidth: 50, idealWidth: 86, alignment: .right),
            TableColumnSpec("relevance", String(localized: "Relevance"), minWidth: 44, idealWidth: 70, alignment: .right),
            TableColumnSpec("files", String(localized: "Files"), minWidth: 60, idealWidth: 200),
        ]
    }

    static func makeController(model: AppModel) -> ListTableController<PeerRow> {
        let properties = model.properties
        let gui = model.gui
        var configuration = ListTableController<PeerRow>.Configuration(
            identifier: "PeerList",
            accessibilityLabel: String(localized: "Peers"),
            specs: specs(showsCountry: model.corePreferences.advanced.resolvePeerCountries),
            layoutKey: GUIPreferenceKey.peerListColumns.rawValue,
            rows: { properties.peers },
            selection: { properties.peerSelection },
            setSelection: { properties.peerSelection = $0 },
            content: { column, row in content(column, row, hideZero: gui.hideZeroValues && gui.hideZeroMode == .always) }
        )
        configuration.sortOrder = { properties.peerSortOrder }
        configuration.setSortOrder = { properties.peerSortOrder = $0 }
        configuration.comparator = comparator
        configuration.menu = { ids in
            guard let item = model.list.currentItem else { return nil }
            return menu(model: model, item: item, ids: ids)
        }
        configuration.deleteAction = { model.requestBanPeers(properties.peerSelection) }
        configuration.copyAction = { model.copyPeers(properties.peerSelection) }
        let controller = ListTableController(configuration)
        controller.observe({ gui.hideZeroValues && gui.hideZeroMode == .always }, apply: { [weak controller] _ in
            controller?.refreshAll()
        })
        return controller
    }

    private static func content(_ column: String, _ row: PeerRow, hideZero: Bool) -> TableCellContent {
        let info = row.info
        func value(_ number: Int64, speed: Bool) -> TableCellContent {
            let text = hideZero && number <= 0 ? "" : (speed ? UnitFormatter.speed(number) : UnitFormatter.size(number))
            return TableCellContent(text, monospacedDigits: true, alignment: .right)
        }
        switch column {
        case "country":
            return TableCellContent(row.flag.isEmpty ? "–" : row.flag, toolTip: info.country.isEmpty ? nil : info.country)
        case "ip":
            return TableCellContent(info.hostName.isEmpty ? info.ip : info.hostName, isSelectable: true, toolTip: info.ip)
        case "port": return TableCellContent(String(info.port), monospacedDigits: true, alignment: .right)
        case "connection": return TableCellContent(info.connectionType)
        case "flags":
            return TableCellContent(info.flags, monospaced: true, toolTip: info.flagsDescription.isEmpty ? nil : info.flagsDescription)
        case "client": return TableCellContent(info.client, toolTip: info.client.isEmpty ? nil : info.client)
        case "peerID": return TableCellContent(info.peerIdClient, monospaced: true)
        case "progress": return TableCellContent(ValueFormatter.percent(info.progress), monospacedDigits: true, alignment: .right)
        case "down": return value(info.dlSpeed, speed: true)
        case "up": return value(info.upSpeed, speed: true)
        case "downloaded": return value(info.downloaded, speed: false)
        case "uploaded": return value(info.uploaded, speed: false)
        case "relevance": return TableCellContent(ValueFormatter.percent(info.relevance), monospacedDigits: true, alignment: .right)
        case "files":
            return TableCellContent(info.files.replacing("\n", with: ";"), toolTip: info.files.isEmpty ? nil : info.files)
        default: return TableCellContent()
        }
    }

    private static func comparator(_ key: String, _ order: SortOrder) -> KeyPathComparator<PeerRow>? {
        switch key {
        case "country": KeyPathComparator(\PeerRow.info.country, order: order)
        case "ip": KeyPathComparator(\PeerRow.info.ip, order: order)
        case "port": KeyPathComparator(\PeerRow.info.port, order: order)
        case "connection": KeyPathComparator(\PeerRow.info.connectionType, order: order)
        case "flags": KeyPathComparator(\PeerRow.info.flags, order: order)
        case "client": KeyPathComparator(\PeerRow.info.client, order: order)
        case "peerID": KeyPathComparator(\PeerRow.info.peerIdClient, order: order)
        case "progress": KeyPathComparator(\PeerRow.info.progress, order: order)
        case "down": KeyPathComparator(\PeerRow.info.dlSpeed, order: order)
        case "up": KeyPathComparator(\PeerRow.info.upSpeed, order: order)
        case "downloaded": KeyPathComparator(\PeerRow.info.downloaded, order: order)
        case "uploaded": KeyPathComparator(\PeerRow.info.uploaded, order: order)
        case "relevance": KeyPathComparator(\PeerRow.info.relevance, order: order)
        case "files": KeyPathComparator(\PeerRow.info.files, order: order)
        default: nil
        }
    }

    /// Peers tab context menu (features.md §2.5.3).
    private static func menu(model: AppModel, item: TorrentItem, ids: Set<String>) -> NSMenu {
        let menu = NSMenu.manual()
        let reason = model.addPeersDisabledReason(item)
        let noSelection = ids.isEmpty ? String(localized: "No peer was selected") : nil
        menu.addAction(String(localized: "Add Peers…"), symbol: Symbol.personBadgePlus, isEnabled: reason == nil, toolTip: reason) {
            model.requestAddPeers(item)
        }
        menu.addAction(String(localized: "Copy IP:port"), symbol: Symbol.docOnDoc, isEnabled: !ids.isEmpty, toolTip: noSelection) {
            model.copyPeers(ids)
        }
        menu.addDivider()
        menu.addAction(String(localized: "Ban Peer Permanently"), symbol: Symbol.nosign, isEnabled: !ids.isEmpty, toolTip: noSelection) {
            model.requestBanPeers(ids)
        }
        return menu
    }
}
