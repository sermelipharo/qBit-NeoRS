import AppKit

/// The editable Path cell of the Manage Torrent Content table. Edits reach
/// the model on every keystroke (so Apply and the italics follow at once);
/// Escape puts back the path the edit started from. Edited paths are drawn
/// in italics, like qBittorrent's `TorrentContentLayoutItemDelegate`.
@MainActor
final class ManageContentPathCell: NSTableCellView, NSTextFieldDelegate {
    static let reuseIdentifier = NSUserInterfaceItemIdentifier("manageContent.path")
    private static let regularFont = NSFont.systemFont(ofSize: NSFont.systemFontSize)
    private static let italicFont = NSFontManager.shared.convert(regularFont, toHaveTrait: .italicFontMask)
    private static let padding: CGFloat = 2

    private let field = NSTextField(string: "")
    private var fileIndex: Int64 = -1
    /// The row's path as the model has it (cleaned).
    private var rowPath = ""
    private var pathBeforeEditing = ""
    /// Called with the file index and the cell's text while it is edited.
    var onEdit: (@MainActor (Int64, String) -> Void)?

    init() {
        super.init(frame: .zero)
        identifier = Self.reuseIdentifier
        field.isBordered = false
        field.isBezeled = false
        field.drawsBackground = false
        field.isEditable = true
        field.font = Self.regularFont
        field.lineBreakMode = .byTruncatingMiddle
        field.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.maximumNumberOfLines = 1
        field.delegate = self
        addSubview(field)
        textField = field
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    static func make(in table: NSTableView) -> ManageContentPathCell {
        table.makeView(withIdentifier: reuseIdentifier, owner: nil) as? ManageContentPathCell ?? ManageContentPathCell()
    }

    func configure(_ row: ManageContentRow) {
        fileIndex = row.id
        rowPath = row.path
        let font = row.isChanged ? Self.italicFont : Self.regularFont
        if field.font != font { field.font = font }
        guard field.currentEditor() == nil, field.stringValue != row.path else { return }
        field.stringValue = row.path
        field.setAccessibilityLabel(row.path)
    }

    override func layout() {
        super.layout()
        let height = ceil(field.intrinsicContentSize.height)
        field.frame = NSRect(x: Self.padding, y: floor((bounds.height - height) / 2),
                             width: max(bounds.width - 2 * Self.padding, 0), height: height)
    }

    // MARK: Editing

    func controlTextDidBeginEditing(_ obj: Notification) {
        pathBeforeEditing = rowPath
    }

    func controlTextDidChange(_ obj: Notification) {
        onEdit?(fileIndex, field.stringValue)
    }

    func controlTextDidEndEditing(_ obj: Notification) {
        onEdit?(fileIndex, field.stringValue)
        field.stringValue = rowPath
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
        guard selector == #selector(NSResponder.cancelOperation(_:)) || selector == #selector(NSTextView.complete(_:)) else {
            return false
        }
        onEdit?(fileIndex, pathBeforeEditing)
        field.abortEditing()
        field.stringValue = pathBeforeEditing
        window?.makeFirstResponder(enclosingTableView)
        return true
    }

    private var enclosingTableView: NSTableView? {
        var view = superview
        while let current = view, !(current is NSTableView) { view = current.superview }
        return view as? NSTableView
    }
}
