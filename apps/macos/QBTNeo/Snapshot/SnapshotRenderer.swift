import AppKit
import SwiftUI

/// `--snapshot <dir>`: renders the main window in several states, the Add
/// Torrent dialog and the other dialogs to PNG files (light and dark) with
/// deterministic demo data, entirely off-screen, then the app exits.
@MainActor
struct SnapshotRenderer {
    let directory: URL
    let launch: LaunchOptions

    func run() async {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        if FeatureSnapshots.isFiltered, FeatureSnapshots.isWanted("main-general") { return await renderGeneralTabVariants() }
        if FeatureSnapshots.isFiltered { return await FeatureSnapshots(directory: directory, launch: launch).run() }
        let suite = "mk.lex.qbitneors.snapshot"
        UserDefaults().removePersistentDomain(forName: suite)
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        let gui = GUIPreferences(defaults: defaults)
        gui.statusBarFreeDiskSpace = true
        gui.propertiesVisible = true
        let model = AppModel(launch: launch, gui: gui)
        let client = DemoClient(large: launch.demoLarge, animated: false)
        model.attach(client)
        await model.refresh()

        let focus = model.transfers.items.first { $0.name.hasPrefix("Sintel") } ?? model.transfers.items.first
        if let focus { model.list.selection = [focus.id] }
        await settle(model)

        for appearance in SnapshotAppearance.allCases {
            gui.propertiesTab = .general
            await settle(model)
            await renderMain(model, name: "main-general", appearance: appearance)
        }
        for tab in [PropertiesTab.trackers, .peers, .httpSources, .content, .speed] {
            gui.propertiesTab = tab
            await settle(model)
            await renderMain(model, name: "main-\(tab.snapshotName)", appearance: .light)
        }
        gui.propertiesTab = .content
        await settle(model)
        await renderMain(model, name: "main-content", appearance: .dark)

        // Filtered list with the panel collapsed.
        gui.propertiesVisible = false
        model.list.statusFilter = .downloading
        model.list.categoryFilter = .category("Linux ISOs")
        await settle(model)
        await renderMain(model, name: "main-filtered", appearance: .light)
        model.list.statusFilter = .all
        model.list.categoryFilter = .all
        gui.propertiesVisible = true
        if let focus { model.list.selection = [focus.id] }
        await settle(model)

        // The other tabs of the main window.
        for pane in [MainPane.search, .rss, .log] {
            model.show(pane: pane)
            try? await Task.sleep(for: .seconds(1))
            await renderMain(model, name: "main-tab-\(pane)", appearance: .light)
        }
        model.mainPane = .transfers

        await render(UILockScreen().environment(model), size: Design.Window.snapshotSize, name: "lock-screen", appearance: .light)
        await SnapshotDialogs(renderer: self, model: model, client: client).renderAll()
        await FeatureSnapshots(directory: directory, launch: launch).run()
    }

    /// `--snapshot-only main-general`: the General tab at the default size,
    /// with a short properties panel, in a narrow window and for a seeding
    /// torrent, light and dark.
    private func renderGeneralTabVariants() async {
        let suite = "mk.lex.qbitneors.snapshot"
        UserDefaults().removePersistentDomain(forName: suite)
        let defaults = UserDefaults(suiteName: suite) ?? .standard
        let gui = GUIPreferences(defaults: defaults)
        gui.propertiesVisible = true
        gui.propertiesTab = .general
        let model = AppModel(launch: launch, gui: gui)
        model.attach(DemoClient(large: launch.demoLarge, animated: false))
        await model.refresh()
        let downloading = model.transfers.items.first { $0.name.hasPrefix("Sintel") } ?? model.transfers.items.first
        let seeding = model.transfers.items.first { $0.state == .uploading && !$0.comment.isEmpty }
            ?? model.transfers.items.first { $0.state == .uploading }
        let narrow = CGSize(width: Design.Window.mainMinSize.width, height: 700)
        let variants: [(name: String, item: TorrentItem?, size: CGSize, panelHeight: Double)] = [
            ("main-general", downloading, Design.Window.snapshotSize, 0),
            ("main-general-short", downloading, Design.Window.snapshotSize, 250),
            ("main-general-narrow", downloading, narrow, 330),
            ("main-general-seeding", seeding, Design.Window.snapshotSize, 300),
        ]
        for variant in variants {
            gui.propertiesHeight = variant.panelHeight
            if let item = variant.item { model.list.selection = [item.id] }
            await settle(model)
            for appearance in SnapshotAppearance.allCases {
                await renderMain(model, name: variant.name, appearance: appearance, size: variant.size)
            }
        }
        model.list.selection = []
        await settle(model)
        await renderMain(model, name: "main-general-empty", appearance: .light)
    }

    // MARK: Rendering

    func settle(_ model: AppModel) async {
        await model.refreshProperties()
        try? await Task.sleep(for: .milliseconds(150))
    }

    private func renderMain(_ model: AppModel, name: String, appearance: SnapshotAppearance, size: CGSize = Design.Window.snapshotSize) async {
        let view = MainWindow()
            .environment(model)
            .environment(model.gui)
        await render(view, size: size, name: name, appearance: appearance)
    }

    /// Hosts `view` in an off-screen window of `size` (nil height = fitting
    /// height), captures the whole frame including the toolbar, writes a PNG.
    func render<V: View>(_ view: V, size: CGSize, name: String, appearance: SnapshotAppearance, fitHeight: Bool = false) async {
        let controller = NSHostingController(
            rootView: view.environment(\.isSnapshotRendering, true).modifier(SnapshotKeyAppearance(isActive: launch.snapshotActive))
        )
        controller.sceneBridgingOptions = [.toolbars, .title]
        let frame = CGRect(origin: CGPoint(x: -20_000, y: -20_000), size: size)
        let style: NSWindow.StyleMask = [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView]
        let window = launch.snapshotActive
            ? ActiveSnapshotWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
            : SnapshotWindow(contentRect: frame, styleMask: style, backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.tabbingMode = .disallowed
        if launch.snapshotClear {
            window.backgroundColor = .clear
            window.isOpaque = false
        }
        window.appearance = NSAppearance(named: appearance.name)
        window.contentViewController = controller
        window.setContentSize(size)
        window.orderFront(nil)
        try? await Task.sleep(for: .milliseconds(500))
        if fitHeight {
            let fitting = controller.view.fittingSize
            window.setContentSize(CGSize(width: size.width, height: max(fitting.height, 120)))
            try? await Task.sleep(for: .milliseconds(250))
        }
        window.displayIfNeeded()
        if let frameView = window.contentView?.superview, let rep = SnapshotBitmap.capture(frameView) {
            let url = directory.appending(path: "\(name)-\(appearance.rawValue).png")
            try? rep.representation(using: .png, properties: [:])?.write(to: url)
            print("snapshot: \(url.path(percentEncoded: false))")
        }
        window.orderOut(nil)
        window.close()
    }
}
