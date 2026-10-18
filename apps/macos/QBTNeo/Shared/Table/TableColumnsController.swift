import AppKit

/// Installs the columns of a table and keeps the user's layout: show/hide
/// from the header's context menu, reordering and resizing by dragging, all
/// saved to the GUI defaults (`TableColumnLayout`). A column can also be made
/// unavailable by the app (the queue column while queueing is off): it is
/// hidden and left out of the menu, and the user's own choice is kept.
@MainActor
final class TableColumnsController: NSObject, NSMenuDelegate {
    let specs: [TableColumnSpec]
    private let storageKey: String?
    private weak var table: NSTableView?
    private var unavailable: Set<String> = []
    /// The user's hidden flag of columns that are currently unavailable.
    private var userHidden: [String: Bool] = [:]
    private var saveTask: Task<Void, Never>?
    private var isApplying = false

    init(table: NSTableView, specs: [TableColumnSpec], storageKey: String?) {
        self.specs = specs
        self.storageKey = storageKey
        self.table = table
        super.init()
        install(in: table)
    }

    func spec(_ id: String) -> TableColumnSpec? { specs.first { $0.id == id } }

    // MARK: Installation

    private func install(in table: NSTableView) {
        let stored = storageKey.flatMap { TableColumnLayout.load(key: $0) }
        let layout = (stored ?? TableColumnLayout(specs: specs)).reconciled(with: specs)
        isApplying = true
        for entry in layout.columns {
            guard let spec = spec(entry.id) else { continue }
            let column = spec.makeColumn()
            column.width = entry.width
            column.isHidden = entry.isHidden
            table.addTableColumn(column)
        }
        isApplying = false
        let menu = NSMenu()
        menu.delegate = self
        table.headerView?.menu = menu
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(layoutDidChange), name: NSTableView.columnDidMoveNotification, object: table)
        center.addObserver(self, selector: #selector(layoutDidChange), name: NSTableView.columnDidResizeNotification, object: table)
    }

    // MARK: Availability

    /// Hides a column the app does not offer right now, or restores the
    /// user's visibility of it.
    func setAvailable(_ available: Bool, column id: String) {
        guard let column = table?.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(id)) else { return }
        if available {
            guard unavailable.remove(id) != nil else { return }
            setHidden(userHidden.removeValue(forKey: id) ?? false, column: column)
        } else {
            guard unavailable.insert(id).inserted else { return }
            userHidden[id] = column.isHidden
            setHidden(true, column: column)
        }
    }

    private func setHidden(_ hidden: Bool, column: NSTableColumn) {
        guard column.isHidden != hidden else { return }
        isApplying = true
        column.isHidden = hidden
        isApplying = false
    }

    // MARK: Header menu

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard let table else { return }
        // Display order, as in Finder's and Mail's column menus.
        for column in table.tableColumns {
            let id = column.identifier.rawValue
            guard let spec = spec(id), !unavailable.contains(id) else { continue }
            let item = NSMenuItem(title: spec.title, action: #selector(toggleColumn(_:)), keyEquivalent: "")
            item.target = self
            item.representedObject = id
            item.state = column.isHidden ? .off : .on
            if !spec.isHideable { item.action = nil }
            menu.addItem(item)
        }
    }

    @objc private func toggleColumn(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String,
              let column = table?.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(id))
        else { return }
        column.isHidden.toggle()
        scheduleSave()
    }

    // MARK: Persistence

    @objc private func layoutDidChange(_ notification: Notification) {
        guard !isApplying else { return }
        scheduleSave()
    }

    /// The current layout, with the user's choice for unavailable columns.
    var layout: TableColumnLayout {
        let columns = (table?.tableColumns ?? []).map { column in
            let id = column.identifier.rawValue
            return TableColumnLayout.Column(id: id, width: column.width, isHidden: userHidden[id] ?? column.isHidden)
        }
        return TableColumnLayout(columns: columns)
    }

    private func scheduleSave() {
        guard storageKey != nil else { return }
        saveTask?.cancel()
        saveTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            self?.saveNow()
        }
    }

    /// Writes a pending layout change now (called when the table goes away).
    func flush() {
        guard saveTask != nil else { return }
        saveNow()
    }

    private func saveNow() {
        saveTask?.cancel()
        saveTask = nil
        guard let storageKey, table != nil else { return }
        layout.save(key: storageKey)
    }
}
