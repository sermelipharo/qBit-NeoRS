import AppKit

/// Content of the transfer-list cells: the state icon before the name, the
/// progress bar, category and tag labels with their icons and colors, and
/// text with the "Hide zero values" and "Color by state" rules, and the
/// name's title in semibold (`TorrentNamesModel`).
@MainActor
struct TransferCell {
    let gui: GUIPreferences
    let labels: LabelAppearanceStore
    let names: TorrentNamesModel

    private static let textID = NSUserInterfaceItemIdentifier("transfer.text")
    private static let progressID = NSUserInterfaceItemIdentifier("transfer.progress")

    /// A reused or new cell for `column`, configured for `item`.
    func view(for column: TransferColumn, item: TorrentItem, in table: NSTableView) -> NSView {
        if column == .progress {
            let cell = TableProgressCell.make(in: table, identifier: Self.progressID)
            configure(cell, column: column, item: item)
            return cell
        }
        let cell = TableTextCell.make(in: table, identifier: Self.textID)
        configure(cell, column: column, item: item)
        return cell
    }

    /// Updates an existing cell; unchanged content is not redrawn.
    func configure(_ view: NSView, column: TransferColumn, item: TorrentItem) {
        if let progress = view as? TableProgressCell {
            progress.configure(TableProgressCell.Content(
                progress: item.progress,
                tint: gui.stateColorScope.colorsProgressBar ? item.state.nsColor : nil,
                isDimmed: item.state.dimsProgressBar
            ))
        } else if let text = view as? TableTextCell {
            text.configure(content(column, item))
        }
    }

    func content(_ column: TransferColumn, _ item: TorrentItem) -> TableCellContent {
        let color = gui.stateColorScope.colorsText(of: column) ? item.state.nsColor : nil
        switch column {
        case .name:
            let icon = TableCellContent.Icon(symbol: item.state.systemImage, color: item.state.nsColor,
                                             accessibilityDescription: item.state.statusText)
            var content = TableCellContent(item.name, color: color, icon: icon, toolTip: item.name)
            // The title semibold, the technical part regular (a cached parse: a lookup per refresh).
            if names.emphasizesListTitles, let parsed = names.parsed(item.name) {
                content.emphasizedRange = parsed.titleRange
            }
            return content
        case .category where !item.category.isEmpty:
            let appearance = labels.category(item.category)
            let label = TableCellContent.Label(name: item.category, symbol: appearance.resolvedSymbol(.category),
                                               color: appearance.resolvedNSColor(.category))
            return TableCellContent(item.category, color: color, labels: [label], toolTip: item.category)
        case .tags where !item.tags.isEmpty:
            let tags = item.tags.map { name in
                let appearance = labels.tag(name)
                return TableCellContent.Label(name: name, symbol: appearance.resolvedSymbol(.tag), color: appearance.resolvedNSColor(.tag))
            }
            return TableCellContent(item.tags.joined(separator: ", "), color: color, labels: tags,
                                    toolTip: item.tags.joined(separator: ", "))
        default:
            let hideZero = gui.hidesZeroValues(isStoppedDownloading: item.state == .stoppedDownloading)
            let text = TransferCellText.text(column, item, hideZero: hideZero)
            let middle = column == .savePath || column == .incompletePath
            return TableCellContent(
                text, color: color, monospacedDigits: true, alignment: column.isNumeric ? .right : .natural,
                lineBreakMode: middle ? .byTruncatingMiddle : .byTruncatingTail,
                toolTip: TransferCellText.hasTooltip(column) && !text.isEmpty ? text : nil
            )
        }
    }
}
