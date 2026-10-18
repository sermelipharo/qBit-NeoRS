import AppKit
import SwiftUI

/// Content tree of the Add Torrent dialog: Name, Total Size, Priority;
/// double-click renames; Space toggles; filter and Select All / None.
struct AddTorrentContentTree: View {
    let dialog: AddTorrentModel
    @State private var renaming: FileNode?
    @State private var newName = ""
    @State private var renameError: String?
    @Environment(GUIPreferences.self) private var gui

    var body: some View {
        let tree = dialog.tree
        VStack(spacing: 0) {
            FileTreeFilterBar(tree: tree, onSelectAll: { tree.setAllChecked(true) }, onSelectNone: { tree.setAllChecked(false) })
                .padding(.horizontal, Design.Spacing.medium)
                .padding(.vertical, Design.Spacing.small)
                .disabled(!dialog.hasMetadata)
            FileTreeTableView(tree: tree, gui: gui) { configuration }
                .overlay {
                    if !dialog.hasMetadata {
                        AddTorrentMetadataState(dialog: dialog)
                    }
                }
        }
        .alert("Renaming", isPresented: isRenaming) {
            TextField("New name:", text: $newName)
            Button("Rename", action: commitRename)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(renameError ?? String(localized: "New name:"))
        }
    }

    /// Name, Total Size and Priority; double-click renames, the menu sets priorities.
    private var configuration: FileTreeTableController.Configuration {
        let tree = dialog.tree
        let rename = { (node: FileNode) in beginRename(node) }
        var configuration = FileTreeTableController.Configuration(
            identifier: "AddTorrentContent", columns: [.name, .size, .priority], layoutKey: nil,
            nameMinWidth: 200, nameIdealWidth: 340, apply: { _ in }
        )
        configuration.menu = { nodes in
            let menu = NSMenu.manual()
            if nodes.count == 1, let node = nodes.first {
                menu.addAction(String(localized: "Rename…"), symbol: Symbol.pencil) { rename(node) }
                menu.addDivider()
            }
            for choice in FilePriorityChoice.selectable {
                menu.addAction(nodes.count == 1 ? choice.title : choice.menuTitle) { tree.setPriority(choice, for: nodes) }
            }
            menu.addDivider()
            menu.addAction(String(localized: "By Shown File Order")) { tree.prioritizeByShownOrder(nodes) }
            return menu
        }
        configuration.primaryAction = { nodes in
            if let node = nodes.first { rename(node) }
        }
        return configuration
    }

    private var isRenaming: Binding<Bool> {
        Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })
    }

    private func beginRename(_ node: FileNode) {
        newName = node.name
        renameError = nil
        renaming = node
    }

    private func commitRename() {
        guard let node = renaming else { return }
        do {
            try dialog.rename(node, to: newName)
        } catch {
            renameError = error.userMessage
        }
    }
}
