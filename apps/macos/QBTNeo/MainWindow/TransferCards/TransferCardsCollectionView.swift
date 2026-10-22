import AppKit

/// `NSCollectionView` with the transfer list's behaviour: a context menu for
/// the cards it targets (the selection when the clicked card is selected,
/// else that card), double-click and Return as the primary action, Delete
/// (Shift-Delete) and other keys routed to closures. Selection, ⌘A,
/// Shift/⌘-click and arrow keys are the collection view's own.
@MainActor
final class TransferCardsCollectionView: NSCollectionView {
    var menuProvider: ((Set<IndexPath>) -> NSMenu?)?
    var primaryAction: ((Set<IndexPath>) -> Void)?
    var deleteAction: ((_ shift: Bool) -> Void)?
    var pasteAction: (() -> Void)?
    var keyHandler: ((NSEvent) -> Bool)?
    /// Called when the emphasis (key window and focus) of the selection changes.
    var onEmphasisChange: ((Bool) -> Void)?

    private var menuTargets: Set<IndexPath> = []
    private var windowObservers: [NSObjectProtocol] = []

    /// Selected cards draw an accent outline while the window is key and this view has focus.
    var isEmphasized: Bool {
        guard let window else { return false }
        return window.isKeyWindow && window.firstResponder === self
    }

    // MARK: Context menu

    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        var targets: Set<IndexPath> = []
        if let path = indexPathForItem(at: point) {
            targets = selectionIndexPaths.contains(path) ? selectionIndexPaths : [path]
        }
        guard let menu = menuProvider?(targets) else { return nil }
        setMenuTargets(targets)
        return menu
    }

    override func didCloseMenu(_ menu: NSMenu, with event: NSEvent?) {
        super.didCloseMenu(menu, with: event)
        setMenuTargets([])
    }

    private func setMenuTargets(_ targets: Set<IndexPath>) {
        for path in menuTargets.union(targets) {
            (item(at: path) as? TransferCardItem)?.card.isMenuTarget = targets.contains(path)
        }
        menuTargets = targets
    }

    // MARK: Mouse and keys

    override func mouseDown(with event: NSEvent) {
        super.mouseDown(with: event)
        guard event.clickCount == 2 else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard let path = indexPathForItem(at: point) else { return }
        primaryAction?(selectionIndexPaths.contains(path) ? selectionIndexPaths : [path])
    }

    override func keyDown(with event: NSEvent) {
        if let keyHandler, keyHandler(event) { return }
        let modifiers = event.modifierFlags.intersection([.command, .option, .control, .shift])
        switch event.specialKey {
        case .carriageReturn?, .enter?, .newline?:
            if modifiers.isEmpty, let primaryAction, !selectionIndexPaths.isEmpty {
                primaryAction(selectionIndexPaths)
                return
            }
        case .delete?, .deleteForward?, .backspace?:
            if let deleteAction, modifiers.isSubset(of: [.shift]), !selectionIndexPaths.isEmpty {
                deleteAction(modifiers.contains(.shift))
                return
            }
        default:
            break
        }
        super.keyDown(with: event)
    }

    /// ⌘A (Edit › Select All). The collection view selects the items without telling its delegate, so tell it.
    override func selectAll(_ sender: Any?) {
        super.selectAll(sender)
        delegate?.collectionView?(self, didSelectItemsAt: selectionIndexPaths)
    }

    @objc func delete(_ sender: Any?) {
        deleteAction?(false)
    }

    @objc func paste(_ sender: Any?) {
        pasteAction?()
    }

    override func responds(to aSelector: Selector!) -> Bool {
        MainActor.assumeIsolated {
            if aSelector == #selector(delete(_:)) { return deleteAction != nil && !selectionIndexPaths.isEmpty }
            if aSelector == #selector(paste(_:)) { return pasteAction != nil }
            return super.responds(to: aSelector)
        }
    }

    // MARK: Emphasis

    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted { notifyEmphasis() }
        return accepted
    }

    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { DispatchQueue.main.async { [weak self] in self?.notifyEmphasis() } }
        return resigned
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        windowObservers.forEach(NotificationCenter.default.removeObserver)
        windowObservers = []
        guard let window else { return }
        for name in [NSWindow.didBecomeKeyNotification, NSWindow.didResignKeyNotification] {
            windowObservers.append(NotificationCenter.default.addObserver(forName: name, object: window, queue: .main) { [weak self] _ in
                MainActor.assumeIsolated { self?.notifyEmphasis() }
            })
        }
    }

    private func notifyEmphasis() {
        onEmphasisChange?(isEmphasized)
    }
}
