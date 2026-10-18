import AppKit

/// Builds a card's content from a torrent: the parsed title, kind and tags
/// (`TorrentNamesModel`, cached per name), category and tags, state,
/// progress, sizes, ratio, speeds and ETA. Runs only for realized cards;
/// `layoutInput` is the cheap part that decides a card's height.
@MainActor
struct TransferCardBuilder {
    let gui: GUIPreferences
    let labels: LabelAppearanceStore
    let names: TorrentNamesModel

    func content(_ item: TorrentItem) -> TransferCardContent {
        var content = TransferCardContent()
        let input = layoutInput(item)
        content.title = input.title
        content.name = item.name
        content.kind = names.contentKind(name: item.name, category: item.category)
        content.chips = input.chips
        content.labels = input.labels
        content.hasCategory = !item.category.isEmpty
        content.stateSymbol = item.state.systemImage
        content.stateColor = item.state.nsColor
        content.statusText = item.statusText
        content.statusTextColor = gui.stateColorScope.colorsText(of: .status) ? item.state.cardTextNSColor : nil
        content.progress = item.progress
        content.progressTint = gui.stateColorScope.colorsProgressBar ? item.state.nsColor : nil
        content.dimsProgress = item.state.dimsProgressBar
        content.percentText = ValueFormatter.progress(item.progress)
        if item.progress >= 1 || item.completed >= item.size {
            content.sizeText = UnitFormatter.size(item.size)
        } else {
            content.sizeText = String(localized: "cards.sizeOf", defaultValue: "\(UnitFormatter.size(item.completed)) of \(UnitFormatter.size(item.size))",
                                      comment: "Card: downloaded of total size")
        }
        if item.dlSpeed > 0 { content.downloadText = "↓ " + UnitFormatter.speed(item.dlSpeed) }
        if item.upSpeed > 0 { content.uploadText = "↑ " + UnitFormatter.speed(item.upSpeed) }
        if Self.showsETA(item) {
            content.etaText = String(localized: "cards.eta", defaultValue: "\(DurationFormatter.eta(item.eta)) left",
                                     comment: "Card: estimated time left")
        }
        content.ratioText = String(localized: "cards.ratio", defaultValue: "Ratio \(ValueFormatter.ratio(item.ratio))",
                                   comment: "Card: share ratio of a finished torrent")
        return content
    }

    /// An ETA is shown while downloading with a known, finite estimate.
    private static func showsETA(_ item: TorrentItem) -> Bool {
        !item.isFinished && item.progress < 1 && item.eta >= 0 && item.eta < DurationFormatter.maxETA && item.dlSpeed > 0
    }

    /// What decides a card's height: the title, the name's tags, the category
    /// and tags, and whether speeds or an ETA need the second status line.
    func layoutInput(_ item: TorrentItem) -> TransferCardLayoutInput {
        let parsed = names.parsed(item.name)
        var result = TransferCardLayoutInput(title: parsed?.title ?? item.name, chips: names.showsCardTags ? parsed?.chips ?? [] : [],
                                             hasActivity: item.dlSpeed > 0 || item.upSpeed > 0 || Self.showsETA(item))
        if !item.category.isEmpty {
            let appearance = labels.category(item.category)
            result.labels.append(TableCellContent.Label(name: item.category, symbol: appearance.resolvedSymbol(.category),
                                                        color: appearance.resolvedNSColor(.category)))
        }
        for tag in item.tags {
            let appearance = labels.tag(tag)
            result.labels.append(TableCellContent.Label(name: tag, symbol: appearance.resolvedSymbol(.tag), color: appearance.resolvedNSColor(.tag)))
        }
        return result
    }
}
