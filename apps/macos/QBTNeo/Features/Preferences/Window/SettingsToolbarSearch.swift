import AppKit
import SwiftUI

/// Adds a native search field at the trailing end of the Settings window's
/// pane toolbar (classic System Preferences). It rests as a magnifying-glass
/// button and expands on click or ⌘F; it collapses again when it loses focus
/// or Escape is pressed while empty (`NSSearchToolbarItem`'s own behaviour). SwiftUI can't do it: on a
/// `Settings` scene's `TabView`, `.searchable` blanks the pane while the field
/// is idle and `.toolbar` items turn the pane buttons into a plain group, so
/// this thin wrapper around the toolbar's delegate supplies one extra
/// `NSSearchToolbarItem` and forwards everything else to SwiftUI.
/// Return opens the first result, Escape clears, ⌘F focuses (`focusRequest`).
/// SwiftUI rebuilds the whole toolbar whenever the query swaps the pane for the
/// results, which removes the field from the window and ends the editing. So
/// the text reaches SwiftUI only once typing pauses, and the field takes its
/// focus and caret back when the toolbar has been rebuilt.
struct SettingsToolbarSearch: NSViewRepresentable {
    @Binding var text: String
    var focusRequest: Int
    var onSubmit: () -> Void

    func makeNSView(context: Context) -> InstallerView {
        let view = InstallerView()
        view.coordinator = context.coordinator
        context.coordinator.installer = view
        return view
    }

    func updateNSView(_ view: InstallerView, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        if !coordinator.isPushPending, coordinator.item.searchField.stringValue != text { coordinator.item.searchField.stringValue = text }
        if coordinator.focusRequest != focusRequest {
            coordinator.focusRequest = focusRequest
            coordinator.expand()
        }
        if let window = view.window { coordinator.install(in: window) }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    final class InstallerView: NSView {
        weak var coordinator: Coordinator?

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            guard let window else { return }
            MainActor.assumeIsolated { coordinator?.install(in: window) }
        }

        /// SwiftUI drops the search item while it lays out the window; putting it back from here, in the
        /// same layout pass, keeps the frame being drawn from showing the toolbar without it.
        override func layout() {
            super.layout()
            MainActor.assumeIsolated { coordinator?.restoreSearchItem() }
        }
    }

    @MainActor
    final class Coordinator: NSObject, NSToolbarDelegate, NSSearchFieldDelegate {
        static let identifier = NSToolbarItem.Identifier("SettingsSearch")
        var parent: SettingsToolbarSearch
        var focusRequest: Int
        let item = NSSearchToolbarItem(itemIdentifier: identifier)
        /// SwiftUI's toolbar delegate (the toolbar holds its delegate weakly; SwiftUI keeps it alive).
        /// Read by the Objective-C forwarding overrides, which AppKit calls on the main thread.
        nonisolated(unsafe) private weak var original: (any NSToolbarDelegate)?
        private var isInstalling = false
        weak var installer: InstallerView?
        /// The toolbar SwiftUI just took the search field out of.
        private weak var toolbarMissingSearch: NSToolbar?
        /// Typing pause before the query is handed to SwiftUI.
        static let typingPause = Duration.milliseconds(400)
        private var pushTask: Task<Void, Never>?
        var isPushPending: Bool { pushTask != nil }
        /// Caret of the field editor that the toolbar rebuild removed, to be put back with the focus.
        private var restoredSelection: NSRange?

        init(parent: SettingsToolbarSearch) {
            self.parent = parent
            self.focusRequest = parent.focusRequest
            super.init()
            item.preferredWidthForSearchField = SettingsLayout.searchFieldWidth
            // No caption under the field: the pane buttons carry labels, the search field doesn't need one.
            item.label = ""
            item.searchField.placeholderString = String(localized: "Search")
            item.searchField.delegate = self
            item.searchField.sendsSearchStringImmediately = false
            item.searchField.target = self
            item.searchField.action = #selector(searchFieldAction(_:))
            item.searchField.setAccessibilityLabel(String(localized: "Search settings"))
            item.resignsFirstResponderWithCancel = true
        }

        /// The item's width. The Settings toolbar centers the pane buttons in the room the trailing items
        /// leave, and it measures that room again only when a constraint changes, not when the item
        /// expands or collapses itself. So this constraint follows the field's focus, and the pane buttons
        /// glide aside instead of the expanded field running over them.
        private var itemWidth: NSLayoutConstraint?
        private var focusObservation: NSKeyValueObservation?
        private var frameObserver: (any NSObjectProtocol)?
        private var lastItemWidth: Double?

        func collapse(animated: Bool = true) {
            item.endSearchInteraction()
            setItemWidth(Self.collapsedItemWidth, animated: animated)
        }

        /// Widens the item and moves the keyboard focus into the field.
        func expand() {
            item.beginSearchInteraction()
        }

        static let collapsedItemWidth = 28.5

        /// Sets the width constraint, animating the toolbar's layout (the pane buttons move with it).
        private func setItemWidth(_ width: Double, animated: Bool = true) {
            guard let itemView = item.searchField.superview else { return }
            if itemWidth?.firstItem !== itemView { attachWidth(to: itemView, width: width) }
            guard let constraint = itemWidth, abs(constraint.constant - width) > 0.5 else { return }
            guard animated, let frame = itemView.window?.contentView?.superview else {
                constraint.constant = width
                return
            }
            NSAnimationContext.runAnimationGroup { context in
                context.duration = 0.2
                context.allowsImplicitAnimation = true
                constraint.constant = width
                frame.layoutSubtreeIfNeeded()
            }
        }

        private func attachWidth(to itemView: NSView, width: Double) {
            itemWidth?.isActive = false
            let constraint = itemView.widthAnchor.constraint(equalToConstant: width)
            constraint.priority = .init(800)
            constraint.isActive = true
            itemWidth = constraint
            // The item starts widening before the field takes the focus, and it can collapse without a focus
            // change (an expansion through accessibility): follow its own width too.
            if let frameObserver { NotificationCenter.default.removeObserver(frameObserver) }
            itemView.postsFrameChangedNotifications = true
            lastItemWidth = itemView.frame.width
            frameObserver = NotificationCenter.default.addObserver(
                forName: NSView.frameDidChangeNotification, object: itemView, queue: .main
            ) { [weak self, weak itemView] _ in
                MainActor.assumeIsolated {
                    guard let self, let itemView else { return }
                    let width = itemView.frame.width
                    // The direction of the item's own animation, step to step.
                    let previous = self.lastItemWidth
                    self.lastItemWidth = width
                    // A view put back into the toolbar starts from no width: that is no expansion.
                    guard let previous, previous >= Self.collapsedItemWidth - 0.5, abs(width - previous) > 0.5 else { return }
                    self.setItemWidth(width > previous ? SettingsLayout.searchFieldWidth : Self.collapsedItemWidth)
                }
            }
        }

        /// The field expands when it takes the keyboard focus (a click on the magnifying glass, ⌘F) and
        /// collapses when it loses it empty: follow the window's first responder.
        private func observeFocus(in window: NSWindow) {
            focusObservation = window.observe(\.firstResponder) { [weak self] _, _ in
                MainActor.assumeIsolated { self?.focusChanged() }
            }
        }

        private func focusChanged() {
            let field = item.searchField
            if field.currentEditor() != nil {
                setItemWidth(SettingsLayout.searchFieldWidth)
            } else if field.stringValue.isEmpty, restoredSelection == nil, !isPushPending, field.window != nil {
                setItemWidth(Self.collapsedItemWidth)
            }
        }

        /// Wraps the toolbar's delegate once the scene has created the toolbar (retrying briefly).
        func install(in window: NSWindow) {
            guard window.toolbar?.delegate !== self, !isInstalling else { return }
            guard let toolbar = window.toolbar else {
                isInstalling = true
                Task { [weak self, weak window] in
                    for _ in 0..<30 where window?.toolbar == nil {
                        try? await Task.sleep(for: .milliseconds(50))
                    }
                    self?.isInstalling = false
                    if let window, window.toolbar != nil { self?.install(in: window) }
                }
                return
            }
            original = toolbar.delegate
            toolbar.delegate = self
            observeFocus(in: window)
            insertSearchItem(into: toolbar)
            if parent.text.isEmpty { collapse(animated: false) }
        }

        /// Appends the flexible space and the search field unless they're there.
        private func insertSearchItem(into toolbar: NSToolbar) {
            guard !toolbar.items.contains(where: { $0.itemIdentifier == Self.identifier }) else { return }
            toolbar.insertItem(withItemIdentifier: .flexibleSpace, at: toolbar.items.count)
            toolbar.insertItem(withItemIdentifier: Self.identifier, at: toolbar.items.count)
        }

        // MARK: NSToolbarDelegate (everything but the search item goes to SwiftUI)

        func toolbar(
            _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier, willBeInsertedIntoToolbar flag: Bool
        ) -> NSToolbarItem? {
            if itemIdentifier == Self.identifier { return item }
            return original?.toolbar?(toolbar, itemForItemIdentifier: itemIdentifier, willBeInsertedIntoToolbar: flag)
        }

        func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            (original?.toolbarDefaultItemIdentifiers?(toolbar) ?? []) + [.flexibleSpace, Self.identifier]
        }

        func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            (original?.toolbarAllowedItemIdentifiers?(toolbar) ?? []) + [.flexibleSpace, Self.identifier]
        }

        func toolbarSelectableItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
            original?.toolbarSelectableItemIdentifiers?(toolbar) ?? []
        }

        func toolbarWillAddItem(_ notification: Notification) {
            original?.toolbarWillAddItem?(notification)
            if (notification.userInfo?["item"] as? NSToolbarItem) === item, restoredSelection != nil { restoreEditing() }
        }

        /// Gives the field the keyboard focus (and caret) it had before the toolbar rebuilt itself.
        private func restoreEditing() {
            Task { [weak self] in
                for _ in 0..<40 {
                    try? await Task.sleep(for: .milliseconds(5))
                    guard let self, let selection = restoredSelection else { return }
                    guard let window = item.searchField.window else { continue }
                    restoredSelection = nil
                    expand()
                    guard item.searchField.currentEditor() != nil || window.makeFirstResponder(item.searchField),
                          let editor = item.searchField.currentEditor() else { return }
                    let length = (editor.string as NSString).length
                    let location = min(selection.location, length)
                    editor.selectedRange = NSRange(location: location, length: min(selection.length, length - location))
                    return
                }
                self?.restoredSelection = nil
            }
        }

        /// SwiftUI updates the pane items (on every pane switch) while it lays out the window, inside the
        /// display cycle, and drops ours with them. Putting the field back on the next turn of the run loop
        /// would let that frame be drawn without it, the pane buttons shifted into its place, so it goes
        /// back in the same layout pass (the installer view lays out after SwiftUI's hosting view), with
        /// the next turn as a fallback.
        func toolbarDidRemoveItem(_ notification: Notification) {
            original?.toolbarDidRemoveItem?(notification)
            let removed = notification.userInfo?["item"] as? NSToolbarItem
            if removed === item, let editor = item.searchField.currentEditor() { restoredSelection = editor.selectedRange }
            guard removed === item, let toolbar = notification.object as? NSToolbar else { return }
            toolbarMissingSearch = toolbar
            installer?.needsLayout = true
            Task { [weak self] in self?.restoreSearchItem() }
        }

        /// Puts the flexible space and the search field back after SwiftUI removed them.
        func restoreSearchItem() {
            guard let toolbar = toolbarMissingSearch else { return }
            toolbarMissingSearch = nil
            guard toolbar.delegate === self else { return }
            if let index = toolbar.items.firstIndex(where: { $0.itemIdentifier == .flexibleSpace }), index == toolbar.items.count - 1 {
                toolbar.removeItem(at: index)
            }
            insertSearchItem(into: toolbar)
        }

        override func responds(to selector: Selector!) -> Bool {
            super.responds(to: selector) || (original?.responds(to: selector) ?? false)
        }

        override func forwardingTarget(for selector: Selector!) -> Any? {
            original?.responds(to: selector) == true ? original : super.forwardingTarget(for: selector)
        }

        // MARK: NSSearchFieldDelegate

        func controlTextDidChange(_ notification: Notification) {
            pushTask?.cancel()
            pushTask = Task { [weak self] in
                try? await Task.sleep(for: Self.typingPause)
                guard !Task.isCancelled else { return }
                self?.pushText()
            }
        }

        /// Collapses to the magnifying-glass button when editing ends with nothing typed
        /// (not while SwiftUI is rebuilding the toolbar, which also ends the editing).
        func controlTextDidEndEditing(_ notification: Notification) {
            Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(60))
                guard let self, restoredSelection == nil, item.searchField.stringValue.isEmpty,
                      item.searchField.window != nil, item.searchField.currentEditor() == nil, !isPushPending else { return }
                collapse()
            }
        }

        /// The clear button and the field's delayed action.
        @objc private func searchFieldAction(_ sender: NSSearchField) {
            pushText()
        }

        /// Hands the field's text to SwiftUI now.
        private func pushText() {
            pushTask?.cancel()
            pushTask = nil
            let value = item.searchField.stringValue
            if parent.text != value { parent.text = value }
        }

        func control(_ control: NSControl, textView: NSTextView, doCommandBy selector: Selector) -> Bool {
            switch selector {
            case #selector(NSResponder.insertNewline(_:)):
                pushText()
                parent.onSubmit()
                return true
            case #selector(NSResponder.cancelOperation(_:)):
                pushTask?.cancel()
                pushTask = nil
                item.searchField.stringValue = ""
                parent.text = ""
                collapse()
                return true
            default:
                return false
            }
        }
    }
}
