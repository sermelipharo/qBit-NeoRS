import AppKit
import SwiftUI

/// `--snapshot-only toolbar-sort`: the main window in the list and in the
/// cards (light and dark), printing every toolbar item's frame, so a run
/// shows whether switching views moved the List / Cards switch, and the
/// toolbar's Sort By menu with each item's on/off state, before and after
/// sorting through the list model as the column headers do.
@MainActor
struct ToolbarSortSnapshots {
    let directory: URL

    func run(model: AppModel) async {
        print("snapshot toolbar-sort: initial sort order \(Self.describe(model.list.sortOrder))")
        model.list.selection = []
        var switchFrames: [TransferViewMode: CGRect] = [:]
        for mode in TransferViewMode.allCases {
            model.gui.transferViewMode = mode
            for appearance in [NSAppearance.Name.aqua, .darkAqua] {
                let suffix = appearance == .aqua ? "light" : "dark"
                let window = await show(model, appearance: appearance)
                let frame = logToolbar(window, name: "\(mode)-\(suffix)")
                if appearance == .aqua { switchFrames[mode] = frame }
                write(window, name: "toolbar-sort-\(mode)-\(suffix)")
                window.orderOut(nil)
                window.close()
            }
        }
        let moved = switchFrames[.list] != switchFrames[.cards]
        print("snapshot toolbar-sort: List / Cards switch \(moved ? "MOVED" : "did not move") (list \(String(describing: switchFrames[.list])), "
              + "cards \(String(describing: switchFrames[.cards])))")

        // View › Sort By and the Command Palette's commands, before and after
        // changes made the way the column headers make them.
        let viewMenu = MainMenuController(model: model).makeViewMenu()
        logSortMenu(viewMenu, model: model, name: "initial")
        model.list.sortOrder = model.list.sortOrder.promotingSortKey(TransferSort(.popularity)) { $0.column == $1.column }
        logSortMenu(viewMenu, model: model, name: "header-popularity")
        CommandRegistry[.sortByRatio].perform(CommandContext(selectionOf: model))
        CommandRegistry[.sortDescending].perform(CommandContext(selectionOf: model))
        logSortMenu(viewMenu, model: model, name: "palette-ratio-desc")
        let first = model.list.visibleItems.prefix(3).map { "\($0.name.prefix(24)) \($0.ratio)" }
        print("snapshot toolbar-sort: first rows by ratio desc: \(first)")
        let window = await show(model, appearance: .aqua)
        _ = logToolbar(window, name: "after-ratio")
        window.orderOut(nil)
        window.close()
        // Left as the persisted order: the next run's "initial" line shows it survived the relaunch.
        print("snapshot toolbar-sort: sort order now \(Self.describe(model.list.sortOrder))")
    }

    /// Prints View › Sort By as a menu opening would show it, and the palette's sort commands that are on.
    private func logSortMenu(_ viewMenu: NSMenu, model: AppModel, name: String) {
        guard let menu = viewMenu.items.first(where: { $0.title == TransferSortMenu.title })?.submenu else {
            print("snapshot toolbar-sort \(name): View › Sort By MISSING")
            return
        }
        menu.delegate?.menuNeedsUpdate?(menu)
        let entries = menu.items.compactMap { item -> String? in
            if item.isSeparatorItem { return "|" }
            if item.isHidden { return nil }
            let enabled = (item.target as? NSMenuItemValidation)?.validateMenuItem(item) ?? true
            return "\(item.state == .on ? "✓" : "")\(item.title)\(enabled ? "" : "(disabled)")"
        }
        print("snapshot toolbar-sort \(name): View › Sort By [\(entries.joined(separator: ", "))]")
        let on = CommandRegistry.sortCommands.filter { $0.isOn?(model) == true }.map(\.title)
        print("snapshot toolbar-sort \(name): palette on \(on); order \(Self.describe(model.list.sortOrder))")
    }

    private static func describe(_ order: [TransferSort]) -> String {
        order.map { "\($0.column.rawValue) \($0.order == .forward ? "asc" : "desc")" }.joined(separator: ", ")
    }

    private func show(_ model: AppModel, appearance: NSAppearance.Name) async -> NSWindow {
        let view = MainWindow().environment(model).environment(model.gui)
        let active = LaunchOptions.current.snapshotActive
        let controller = NSHostingController(rootView: view.modifier(SnapshotKeyAppearance(isActive: active)))
        controller.sceneBridgingOptions = [.toolbars, .title]
        let frame = CGRect(origin: CGPoint(x: -20_000, y: -20_000), size: Design.Window.snapshotSize)
        let style: NSWindow.StyleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
        let window = active
            ? ActiveSnapshotWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
            : SnapshotWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        window.appearance = NSAppearance(named: appearance)
        window.contentViewController = controller
        window.setContentSize(Design.Window.snapshotSize)
        window.orderFront(nil)
        try? await Task.sleep(for: .milliseconds(900))
        window.displayIfNeeded()
        return window
    }

    /// Prints the toolbar items and the Sort By menu; returns the switch's frame in the window.
    private func logToolbar(_ window: NSWindow, name: String) -> CGRect? {
        var switchFrame: CGRect?
        for item in window.toolbar?.items ?? [] {
            let frame = item.view.map { $0.convert($0.bounds, to: nil) }
            print("snapshot toolbar-sort \(name): item \(item.itemIdentifier.rawValue) '\(item.label)' frame \(frame.map { "\($0)" } ?? "-")")
            if let menuItem = item as? NSMenuToolbarItem {
                let entries = menuItem.menu.items.map { entry in
                    entry.isSeparatorItem ? "|" : "\(entry.state == .on ? "✓" : "")\(entry.title)\(entry.isHidden ? "(hidden)" : "")"
                }
                print("snapshot toolbar-sort \(name): menu '\(menuItem.label)' [\(entries.joined(separator: ", "))]")
            }
        }
        if let content = window.contentView?.superview {
            for control in Self.segmentedControls(in: content) {
                let frame = control.convert(control.bounds, to: nil)
                print("snapshot toolbar-sort \(name): segmented control frame \(frame)")
                switchFrame = switchFrame ?? frame
            }
        }
        return switchFrame
    }

    private static func segmentedControls(in view: NSView) -> [NSSegmentedControl] {
        view.subviews.flatMap { subview -> [NSSegmentedControl] in
            (subview as? NSSegmentedControl).map { [$0] } ?? segmentedControls(in: subview)
        }
    }

    private func write(_ window: NSWindow, name: String) {
        guard let frameView = window.contentView?.superview, let rep = SnapshotBitmap.capture(frameView),
              let data = rep.representation(using: .png, properties: [:])
        else { return }
        let url = directory.appending(path: "\(name).png")
        try? data.write(to: url)
        print("snapshot: \(url.path(percentEncoded: false))")
    }
}
