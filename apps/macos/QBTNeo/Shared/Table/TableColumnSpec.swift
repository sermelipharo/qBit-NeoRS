import AppKit

/// One column of an AppKit table (`NativeTableView`): identity, header,
/// widths, alignment and whether the user may hide or sort it. The `id` is
/// the stable key of the persisted layout and of the column's sort key.
struct TableColumnSpec: Sendable {
    let id: String
    let title: String
    var headerToolTip: String?
    var minWidth: CGFloat = 40
    var idealWidth: CGFloat = 100
    var maxWidth: CGFloat = 10_000
    var alignment: NSTextAlignment = .natural
    var isVisibleByDefault = true
    /// False for the column that identifies a row (Name, URL): it stays visible.
    var isHideable = true
    var isSortable = true

    init(
        _ id: String, _ title: String, headerToolTip: String? = nil, minWidth: CGFloat = 40, idealWidth: CGFloat = 100,
        maxWidth: CGFloat = 10_000, alignment: NSTextAlignment = .natural, isVisibleByDefault: Bool = true,
        isHideable: Bool = true, isSortable: Bool = true
    ) {
        self.id = id
        self.title = title
        self.headerToolTip = headerToolTip
        self.minWidth = minWidth
        self.idealWidth = idealWidth
        self.maxWidth = maxWidth
        self.alignment = alignment
        self.isVisibleByDefault = isVisibleByDefault
        self.isHideable = isHideable
        self.isSortable = isSortable
    }

    var identifier: NSUserInterfaceItemIdentifier { NSUserInterfaceItemIdentifier(id) }

    /// A configured `NSTableColumn` for this spec (width and visibility come from the layout afterwards).
    @MainActor
    func makeColumn() -> NSTableColumn {
        let column = NSTableColumn(identifier: identifier)
        column.title = title
        column.headerToolTip = headerToolTip
        column.minWidth = minWidth
        column.maxWidth = maxWidth
        column.width = idealWidth
        column.resizingMask = .userResizingMask
        column.headerCell.alignment = alignment
        column.isHidden = !isVisibleByDefault
        if isSortable {
            column.sortDescriptorPrototype = NSSortDescriptor(key: id, ascending: true)
        }
        return column
    }
}
