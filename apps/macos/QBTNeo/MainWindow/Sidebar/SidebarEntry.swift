import AppKit

/// An item of the sidebar outline: a section header or a filter row. The
/// outline tracks items by identity, so entries are kept per key and reused
/// across updates (expansion and selection survive reloads).
@MainActor
final class SidebarEntry: NSObject {
    enum Key: Hashable {
        case section(SidebarItem.Section)
        case row(SidebarItem)
    }

    let key: Key
    var title = ""
    /// Nil for section headers.
    var row: SidebarSnapshot.Row?
    var children: [SidebarEntry] = []

    init(key: Key) {
        self.key = key
    }

    var item: SidebarItem? {
        if case .row(let item) = key { return item }
        return nil
    }

    var section: SidebarItem.Section? {
        if case .section(let section) = key { return section }
        return nil
    }
}
