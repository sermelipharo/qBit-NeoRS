import SwiftUI

/// Main window toolbar (features.md §1.2): add, remove, start/stop, queue
/// (when queueing is enabled) and options; the filter field is `.searchable`.
/// Buttons render from the command registry, like the menus. The List /
/// Cards switch (a qBit NeoRS extension) is the first trailing group, like
/// Finder's view picker. The add buttons and the title keep their places;
/// the system gives the search field 41 pt less (1 180 pt window), so the
/// command group moves 41 pt right (placed with the add buttons instead,
/// the title would move as well, and the search field shrinks the same).
struct MainToolbar: ToolbarContent {
    let model: AppModel

    var body: some ToolbarContent {
        ToolbarItemGroup(placement: .navigation) {
            CommandToolbarButton(.addTorrentFile, model: model)
            CommandToolbarButton(.addTorrentLink, model: model)
        }
        ToolbarItem {
            TransferViewModePicker(gui: model.gui)
        }
        ToolbarItemGroup {
            CommandToolbarButton(.remove, model: model)
        }
        ToolbarItemGroup {
            CommandToolbarButton(.start, model: model)
            CommandToolbarButton(.stop, model: model)
        }
        if model.queueingEnabled {
            ToolbarItemGroup {
                CommandToolbarButton(.queueTop, model: model)
                CommandToolbarButton(.queueUp, model: model)
                CommandToolbarButton(.queueDown, model: model)
                CommandToolbarButton(.queueBottom, model: model)
            }
        }
        ToolbarItem {
            CommandToolbarButton(.lock, model: model, label: String(localized: "Lock"))
        }
        ToolbarItem {
            CommandToolbarButton(.options, model: model)
        }
    }
}
