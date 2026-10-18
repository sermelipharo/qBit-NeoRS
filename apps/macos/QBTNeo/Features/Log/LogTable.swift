import AppKit

/// The Execution Log list: one header-less column of `<time> - <message>`
/// lines wrapped to at most four lines, colored like qBittorrent's `Log.*`
/// theme colors, with Copy and Clear.
@MainActor
enum LogTable {
    private static let maximumLines = 4

    static func makeController(model: ExecutionLogModel, clear: @escaping @MainActor () -> Void) -> ListTableController<LogLine> {
        var configuration = ListTableController<LogLine>.Configuration(
            identifier: "ExecutionLog",
            accessibilityLabel: String(localized: "Execution Log"),
            specs: [TableColumnSpec("line", String(localized: "Message"), minWidth: 100, idealWidth: 800, isHideable: false,
                                    isSortable: false)],
            layoutKey: nil,
            rows: { model.visibleLines },
            selection: { model.selection },
            setSelection: { model.selection = $0 },
            content: { _, line in content(line) }
        )
        configuration.showsHeader = false
        configuration.rowHeight = { line, width in
            TableTextCell.height(of: content(line), width: width, minimum: 24)
        }
        configuration.menu = { ids in
            let menu = NSMenu.manual()
            menu.addAction(String(localized: "Copy"), symbol: Symbol.docOnDoc, isEnabled: !ids.isEmpty) { model.copySelection() }
            menu.addAction(String(localized: "Clear"), symbol: Symbol.trash) { clear() }
            return menu
        }
        configuration.copyAction = { model.copySelection() }
        return ListTableController(configuration)
    }

    private static func content(_ line: LogLine) -> TableCellContent {
        var content = TableCellContent(line.text, color: color(line.kind))
        content.prefix = line.time + " - "
        content.prefixColor = timestamp
        content.maximumLines = maximumLines
        content.accessibilityLabel = "\(levelName(line.kind)): \(line.text), \(line.time)"
        return content
    }

    private static func color(_ kind: LogLine.Kind) -> NSColor? {
        switch kind {
        case .message(.normal): nil
        case .message(.info): info
        case .message(.warning): warning
        case .message(.critical), .blockedPeer: critical
        }
    }

    private static func levelName(_ kind: LogLine.Kind) -> String {
        switch kind {
        case .message(.normal): String(localized: "Normal")
        case .message(.info): String(localized: "Information")
        case .message(.warning): String(localized: "Warning")
        case .message(.critical): String(localized: "Critical")
        case .blockedPeer: String(localized: "Blocked peer")
        }
    }

    // qBittorrent's `Log.*` theme colors (as `FeatureDesign.Palette`).
    private static let info = NSColor(light: 0x0969DA, dark: 0x58A6FF)
    private static let warning = NSColor(light: 0xBC4C00, dark: 0xDB6D28)
    private static let critical = NSColor(light: 0xCF222E, dark: 0xF85149)
    private static let timestamp = NSColor(light: 0x6E7781, dark: 0x6E7681)
}
