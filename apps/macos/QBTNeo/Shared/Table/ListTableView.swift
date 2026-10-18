import AppKit
import SwiftUI

/// Hosts a `ListTableController` in SwiftUI. The controller is made once and
/// follows its model through observation; dismantling stops it.
struct ListTableView<Row: Identifiable & Equatable & Sendable>: NSViewRepresentable where Row.ID: Sendable {
    let gui: GUIPreferences
    let makeController: @MainActor () -> ListTableController<Row>

    func makeCoordinator() -> ListTableController<Row> {
        makeController()
    }

    func makeNSView(context: Context) -> NSScrollView {
        context.coordinator.start(gui: gui)
        return context.coordinator.scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {}

    static func dismantleNSView(_ nsView: NSScrollView, coordinator: ListTableController<Row>) {
        coordinator.stop()
    }
}
