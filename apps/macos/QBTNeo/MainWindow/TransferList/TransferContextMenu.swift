import AppKit

/// The transfer-list context menu (features.md §1.8), with entries shown,
/// enabled and checked according to the whole selection. Titles and symbols
/// match the command registry and the Edit menu.
@MainActor
struct TransferContextMenu {
    let selection: TorrentSelection

    private var model: AppModel { selection.model }

    func makeMenu() -> NSMenu {
        let menu = NSMenu.manual()
        let ids = selection.ids
        let model = model
        if selection.needsStart {
            menu.addAction(String(localized: "Start"), symbol: Symbol.playFill) { model.start(ids) }
        }
        if selection.needsStop {
            menu.addAction(String(localized: "Stop"), symbol: Symbol.pauseFill) { model.stop(ids) }
        }
        if selection.needsForce {
            menu.addAction(String(localized: "Force Start"), symbol: Symbol.forwardFill) { model.forceStart(ids) }
        }
        menu.addDivider()
        menu.addAction(String(localized: "Remove"), symbol: Symbol.trash) { model.requestRemove(ids) }
        menu.addDivider()
        menu.addAction(String(localized: "Set Location…"), symbol: Symbol.folder) { model.requestSetLocation(ids) }
        if ids.count == 1 {
            menu.addAction(String(localized: "Rename…"), symbol: Symbol.pencil) { model.requestRename(ids[0]) }
        }
        menu.addAction(String(localized: "Edit Trackers…"), symbol: Symbol.antennaRadiowavesLeftAndRight) {
            model.sheet = .editTrackers(ids: ids)
        }
        addCategoryMenu(to: menu)
        addTagsMenu(to: menu)
        let autoTMMHelp = String(localized: "Automatic mode means that various torrent properties (e.g. save path) will be decided by the associated category")
        menu.addToggle(String(localized: "Automatic Torrent Management"), values: [selection.items.allSatisfy(\.autoTMM)], toolTip: autoTMMHelp) {
            model.setAutoTMM(ids, enabled: $0)
        }
        menu.addDivider()
        menu.addAction(String(localized: "Torrent Options…"), symbol: Symbol.sliderHorizontal3) { model.sheet = .torrentOptions(ids: ids) }
        if selection.allFinishedWithMetadata {
            let items = selection.items.filter(\.hasMetadata)
            menu.addToggle(String(localized: "Super Seeding Mode"), values: items.map(\.superSeeding)) {
                model.setSuperSeeding(items.map(\.id), $0)
            }
        }
        menu.addDivider()
        if selection.hasMetadata {
            menu.addAction(String(localized: "Preview File…"), symbol: Symbol.eye) { model.requestPreview(ids[0]) }
        }
        if selection.hasUnfinished {
            let unfinished = selection.items.filter { !$0.isFinished }
            let unfinishedIDs = unfinished.map(\.id)
            menu.addToggle(String(localized: "Download in Sequential Order"), values: unfinished.map(\.sequentialDownload)) {
                model.setSequential(unfinishedIDs, $0)
            }
            menu.addToggle(String(localized: "Download First and Last Pieces First"), values: unfinished.map(\.firstLastPiecePriority)) {
                model.setFirstLastPiece(unfinishedIDs, $0)
            }
        }
        menu.addDivider()
        if selection.hasMetadata {
            menu.addAction(String(localized: "Force Recheck"), symbol: Symbol.arrowTrianglehead2ClockwiseRotate90) { model.requestRecheck(ids) }
        }
        let canReannounce = selection.canReannounce
        menu.addAction(String(localized: "Force Reannounce"), symbol: Symbol.arrowClockwise, isEnabled: canReannounce,
                       toolTip: canReannounce ? nil : String(localized: "Can not force reannounce if torrent is Stopped/Queued/Errored/Checking")) {
            model.reannounce(ids)
        }
        menu.addDivider()
        menu.addAction(String(localized: "Open Destination Folder"), symbol: Symbol.folderBadgeGearshape) { model.openDestination(ids) }
        if model.queueingEnabled, selection.hasUnfinished {
            menu.addSubmenu(String(localized: "Queue"), symbol: Symbol.listNumber) { queue in
                queue.addAction(String(localized: "Move to Top"), symbol: Symbol.arrowUpToLine) { model.queue(ids, .top) }
                queue.addAction(String(localized: "Move Up"), symbol: Symbol.chevronUp) { model.queue(ids, .up) }
                queue.addAction(String(localized: "Move Down"), symbol: Symbol.chevronDown) { model.queue(ids, .down) }
                queue.addAction(String(localized: "Move to Bottom"), symbol: Symbol.arrowDownToLine) { model.queue(ids, .bottom) }
            }
        }
        menu.addSubmenu(String(localized: "Copy"), symbol: Symbol.docOnDoc) { copy in
            for kind in CopyKind.allCases {
                let enabled = !(kind == .infohashV1 && !selection.hasInfohashV1 || kind == .infohashV2 && !selection.hasInfohashV2)
                copy.addAction(kind.title, isEnabled: enabled) { model.copy(kind, ids: ids) }
            }
        }
        menu.addAction(String(localized: "Export .torrent…"), symbol: Symbol.squareAndArrowUp,
                       toolTip: String(localized: "Exported torrent is not necessarily the same as the imported")) {
            model.requestExport(ids)
        }
        menu.trimSeparators()
        return menu
    }

    /// "Category" submenu: New…, Reset, then every category (checked when shared).
    private func addCategoryMenu(to menu: NSMenu) {
        let ids = selection.ids
        let model = model
        let common = selection.commonCategory
        menu.addSubmenu(String(localized: "Category"), symbol: Symbol.folder) { submenu in
            submenu.addAction(String(localized: "New…"), symbol: Symbol.plus) { model.requestNewCategory(assignTo: ids) }
            submenu.addAction(String(localized: "Reset"), symbol: Symbol.xmark) { model.setCategory(ids, "") }
            submenu.addDivider()
            for name in model.categories.sortedNames {
                submenu.addAction(name, state: name == common ? .on : .off) { model.setCategory(ids, name) }
            }
        }
    }

    /// "Tags" submenu: Add…, Remove All, then one tri-state entry per tag.
    private func addTagsMenu(to menu: NSMenu) {
        let ids = selection.ids
        let items = selection.items
        let model = model
        menu.addSubmenu(String(localized: "Tags"), symbol: Symbol.tag) { submenu in
            submenu.addAction(String(localized: "Add…"), symbol: Symbol.plus) { model.requestAddTags(ids) }
            submenu.addAction(String(localized: "Remove All"), symbol: Symbol.xmark) { model.requestRemoveAllTags(ids) }
            submenu.addDivider()
            for tag in model.tags.sortedTags {
                submenu.addToggle(tag, values: items.map { $0.tags.contains(tag) }) { enabled in
                    let targets = items.filter { $0.tags.contains(tag) != enabled }.map(\.id)
                    guard !targets.isEmpty else { return }
                    if enabled {
                        model.perform { try $0.addTags(hashes: targets, tags: [tag]) }
                    } else {
                        model.perform { try $0.removeTags(hashes: targets, tags: [tag]) }
                    }
                }
            }
        }
    }
}
