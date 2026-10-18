import SwiftUI

/// Content tab (features.md §2.7): file tree with progress, priorities,
/// availability, filter, Select All / None and the file context menu. The
/// tree is an AppKit table (`FileTreeTableController`).
struct ContentTab: View {
    let item: TorrentItem
    @Environment(AppModel.self) private var model

    var body: some View {
        let tree = model.properties.files
        VStack(spacing: 0) {
            FileTreeFilterBar(tree: tree, onSelectAll: { apply(tree.setAllChecked(true)) },
                              onSelectNone: { apply(tree.setAllChecked(false)) })
                .padding(.horizontal, Design.Spacing.medium)
                .padding(.vertical, Design.Spacing.small)
            Divider()
            FileTreeTableView(tree: tree, gui: model.gui) { Self.configuration(model: model) }
                .overlay {
                    if tree.rows.isEmpty, tree.isEmpty {
                        if !item.hasMetadata {
                            ContentUnavailableView("Waiting for Metadata", systemImage: Symbol.arrowDownCircleDotted,
                                                   description: Text("The file list appears once metadata has been received."))
                        } else {
                            PropertiesFailureOverlay(tab: .content, isEmpty: true)
                        }
                    }
                }
        }
    }

    private func apply(_ changes: [FilePriority: [Int64]]) {
        model.applyFilePriorities(item.id, changes)
    }

    /// Table setup; actions target the torrent shown at the time they run.
    @MainActor
    private static func configuration(model: AppModel) -> FileTreeTableController.Configuration {
        let apply: @MainActor ([FilePriority: [Int64]]) -> Void = { changes in
            guard let item = model.list.currentItem else { return }
            model.applyFilePriorities(item.id, changes)
        }
        var configuration = FileTreeTableController.Configuration(
            identifier: "ContentList", columns: FileColumn.allCases, layoutKey: GUIPreferenceKey.contentListColumns.rawValue,
            apply: apply
        )
        configuration.menu = { nodes in
            guard let item = model.list.currentItem else { return nil }
            return ContentContextMenu(model: model, item: item, nodes: nodes, apply: apply).makeMenu()
        }
        configuration.primaryAction = { nodes in
            guard nodes.count == 1, let node = nodes.first, let item = model.list.currentItem else { return }
            model.openContent(item, node)
        }
        return configuration
    }
}
