import AppKit

/// `NSTableView` with the behaviour every list of the app shares: a context
/// menu built for the rows it targets, Return/Enter as the primary action,
/// Delete (Shift-Delete) and Copy/Paste routed to closures, and alternating
/// rows that are never drawn behind an empty table.
@MainActor
final class NativeTableView: NSTableView {
    /// Builds the context menu for the rows a right-click targets: the
    /// selection when the clicked row is selected, else that row alone.
    /// An empty set means a click below the rows.
    var menuProvider: ((IndexSet) -> NSMenu?)?
    /// Double-click on a row, Return or Enter, with the target rows.
    var primaryAction: ((IndexSet) -> Void)?
    /// Delete / Forward Delete; the flag is true with Shift held.
    var deleteAction: ((_ shift: Bool) -> Void)?
    var copyAction: (() -> Void)?
    var pasteAction: (() -> Void)?
    /// Other keys (F2 for rename); returns true when handled.
    var keyHandler: ((NSEvent) -> Bool)?

    /// The user's "Alternating row colors" setting; stripes are only drawn
    /// when there are rows.
    var wantsAlternatingRows = false {
        didSet { updateStripes() }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        target = self
        doubleAction = #selector(doubleClicked)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func reloadData() {
        super.reloadData()
        updateStripes()
    }

    override func noteNumberOfRowsChanged() {
        super.noteNumberOfRowsChanged()
        updateStripes()
    }

    func updateStripes() {
        let stripes = wantsAlternatingRows && numberOfRows > 0
        if usesAlternatingRowBackgroundColors != stripes { usesAlternatingRowBackgroundColors = stripes }
    }

    // MARK: Context menu

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = row(at: convert(event.locationInWindow, from: nil))
        let rows: IndexSet
        if row < 0 {
            rows = []
        } else if selectedRowIndexes.contains(row) {
            rows = selectedRowIndexes
        } else {
            rows = IndexSet(integer: row)
        }
        menu = menuProvider?(rows)
        // `super` records `clickedRow` and draws the focus ring around the targeted rows.
        return super.menu(for: event)
    }

    /// The rows a context-menu action acts on (valid while the menu is open
    /// or its action runs).
    var menuTargetRows: IndexSet {
        let row = clickedRow
        guard row >= 0 else { return selectedRowIndexes }
        return selectedRowIndexes.contains(row) ? selectedRowIndexes : IndexSet(integer: row)
    }

    /// Selectable labels (IP addresses) take the click for text selection once their row is selected.
    override func validateProposedFirstResponder(_ responder: NSResponder, for event: NSEvent?) -> Bool {
        if let field = responder as? NSTextField, field.isSelectable, !field.isEditable,
           selectedRowIndexes.contains(row(for: field)) {
            return true
        }
        return super.validateProposedFirstResponder(responder, for: event)
    }

    // MARK: Actions

    @objc private func doubleClicked() {
        let row = clickedRow
        guard row >= 0 else { return }
        primaryAction?(selectedRowIndexes.contains(row) ? selectedRowIndexes : IndexSet(integer: row))
    }

    override func keyDown(with event: NSEvent) {
        if let keyHandler, keyHandler(event) { return }
        let modifiers = event.modifierFlags.intersection([.command, .option, .control, .shift])
        switch event.specialKey {
        case .carriageReturn?, .enter?, .newline?:
            if modifiers.isEmpty, let primaryAction, !selectedRowIndexes.isEmpty {
                primaryAction(selectedRowIndexes)
                return
            }
        case .delete?, .deleteForward?, .backspace?:
            if let deleteAction, modifiers.isSubset(of: [.shift]), !selectedRowIndexes.isEmpty {
                deleteAction(modifiers.contains(.shift))
                return
            }
        default:
            break
        }
        super.keyDown(with: event)
    }

    @objc func copy(_ sender: Any?) {
        copyAction?()
    }

    @objc func paste(_ sender: Any?) {
        pasteAction?()
    }

    @objc func delete(_ sender: Any?) {
        deleteAction?(false)
    }

    override func responds(to aSelector: Selector!) -> Bool {
        MainActor.assumeIsolated {
            switch aSelector {
            case #selector(copy(_:)): copyAction != nil && !selectedRowIndexes.isEmpty
            case #selector(paste(_:)): pasteAction != nil
            case #selector(delete(_:)): deleteAction != nil && !selectedRowIndexes.isEmpty
            default: super.responds(to: aSelector)
            }
        }
    }
}
