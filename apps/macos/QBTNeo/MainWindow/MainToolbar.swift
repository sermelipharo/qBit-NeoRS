import SwiftUI

/// Main window toolbar (features.md §1.2), customizable (View › Customize
/// Toolbar…). On the Transfers tab: add, the List / Cards switch (a qBit
/// NeoRS extension, like Finder's view picker), remove, start/stop, queue
/// (when queueing is enabled), lock and options; buttons render from the
/// command registry, like the menus. The RSS tab shows the reader's actions,
/// the full-size log its General / Blocked IPs switch. The search field is
/// the window's `.searchable`.
struct MainToolbar: CustomizableToolbarContent {
    let model: AppModel

    var body: some CustomizableToolbarContent {
        ToolbarItem(id: "add", placement: .navigation) {
            ControlGroup {
                CommandToolbarButton(.addTorrentFile, model: model)
                CommandToolbarButton(.addTorrentLink, model: model)
            }
        }
        .customizationBehavior(.reorderable)
        switch model.mainPane {
        case .transfers:
            transferItems
        case .rss:
            rssItems
        case .log:
            ToolbarItem(id: "log.tabs", placement: .principal) {
                LogTabPicker(model: model.logTab)
            }
            .customizationBehavior(.disabled)
        case .search:
            ToolbarItem(id: "search.plugins") {
                Button("Search Plugins…", systemImage: Symbol.puzzlepieceExtension) {
                    model.openWindow?(id: FeatureWindowID.searchPlugins.rawValue)
                }
                .help("Search Plugins…")
            }
        }
    }

    @ToolbarContentBuilder private var transferItems: some CustomizableToolbarContent {
        ToolbarItem(id: "viewMode") {
            TransferViewModePicker(gui: model.gui)
        }
        ToolbarItem(id: "remove") {
            CommandToolbarButton(.remove, model: model)
        }
        ToolbarItem(id: "startStop") {
            ControlGroup {
                CommandToolbarButton(.start, model: model)
                CommandToolbarButton(.stop, model: model)
            }
        }
        ToolbarItem(id: "queue") {
            if model.queueingEnabled {
                ControlGroup {
                    CommandToolbarButton(.queueTop, model: model)
                    CommandToolbarButton(.queueUp, model: model)
                    CommandToolbarButton(.queueDown, model: model)
                    CommandToolbarButton(.queueBottom, model: model)
                }
            }
        }
        ToolbarItem(id: "forceStart", showsByDefault: false) {
            CommandToolbarButton(.forceStart, model: model)
        }
        ToolbarItem(id: "recheck", showsByDefault: false) {
            CommandToolbarButton(.forceRecheck, model: model)
        }
        ToolbarItem(id: "altSpeed", showsByDefault: false) {
            CommandToolbarButton(.alternativeSpeedLimits, model: model)
        }
        ToolbarItem(id: "lock") {
            CommandToolbarButton(.lock, model: model, label: String(localized: "Lock"))
        }
        ToolbarItem(id: "options") {
            CommandToolbarButton(.options, model: model)
        }
    }

    @ToolbarContentBuilder private var rssItems: some CustomizableToolbarContent {
        let rss = model.rssTab
        ToolbarItem(id: "rss.update") {
            ControlGroup {
                Button("New subscription", systemImage: Symbol.plus) {
                    rss.sheet = .newSubscription(url: RSSFeedSheet.clipboardURL())
                }
                .help("New subscription")
                Button("Mark items read", systemImage: Symbol.checkmarkCircle) { rss.markRead(rss.selectedNodes) }
                    .help("Mark items read")
                    .disabled(rss.selection.isEmpty)
                Button("Update all", systemImage: Symbol.arrowClockwise, action: rss.updateAll)
                    .help("Refresh RSS streams")
            }
        }
        ToolbarItem(id: "rss.downloader") {
            Button("RSS Downloader…", systemImage: Symbol.arrowDownDoc) {
                model.openWindow?(id: FeatureWindowID.rssDownloader.rawValue)
            }
            .help("RSS Downloader…")
        }
    }
}
