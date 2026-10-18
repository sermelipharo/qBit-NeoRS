import AppKit
import SwiftUI

/// Hosts a `FileTreeTableController` in SwiftUI.
struct FileTreeTableView: NSViewRepresentable {
    let tree: FileTreeModel
    let gui: GUIPreferences
    let configuration: @MainActor () -> FileTreeTableController.Configuration

    func makeCoordinator() -> FileTreeTableController {
        FileTreeTableController(tree: tree, configuration: configuration())
    }

    func makeNSView(context: Context) -> NSScrollView {
        context.coordinator.start(gui: gui)
        return context.coordinator.scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    static func dismantleNSView(_ nsView: NSScrollView, coordinator: FileTreeTableController) {
        coordinator.stop()
    }
}
