import AppKit

/// The sidebar's outline view: a context menu for the right-clicked row
/// (sidebar menus act on that row, not on the selection).
@MainActor
final class SidebarOutlineView: NSOutlineView {
    var menuProvider: ((Int) -> NSMenu?)?

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = row(at: convert(event.locationInWindow, from: nil))
        menu = row >= 0 ? menuProvider?(row) : nil
        // `super` records `clickedRow` and draws the focus ring around the row.
        return super.menu(for: event)
    }
}
