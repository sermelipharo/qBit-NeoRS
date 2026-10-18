import AppKit

/// Heights of the cards: each card's from its title (one or two lines) and
/// its tags (none, one or two rows), and each grid row's from its tallest
/// card. A card's input is kept with its title's single-line width, so a
/// resize only redoes arithmetic (tag widths are cached by `NameChipLayout`);
/// inputs are refreshed when the torrents, names or labels change.
@MainActor
final class TransferCardHeights {
    private struct Entry {
        var input: TransferCardLayoutInput
        var titleWidth: CGFloat
    }

    private var entries: [String: Entry] = [:]
    private var rows: [CGFloat] = []
    private var rowsKey: (count: Int, columns: Int, width: CGFloat)?

    /// Records a card's input; true when it differs from the recorded one (its height may change).
    @discardableResult
    func update(_ id: String, input: TransferCardLayoutInput) -> Bool {
        if let entry = entries[id], entry.input == input { return false }
        entries[id] = Entry(input: input, titleWidth: TransferCardMetrics.naturalTitleWidth(input.title))
        rowsKey = nil
        return true
    }

    /// Forgets every input (names or labels changed) and every row.
    func removeAll() {
        entries.removeAll(keepingCapacity: true)
        rowsKey = nil
    }

    /// Drops inputs of torrents that are no longer listed.
    func retain(_ ids: Set<String>) {
        guard entries.count > ids.count else { return }
        entries = entries.filter { ids.contains($0.key) }
    }

    /// The height of the row that holds `index`; rows are computed once per item list, column count and width.
    func rowHeight(at index: Int, ids: [String], columns: Int, cardWidth: CGFloat, input: (Int) -> TransferCardLayoutInput) -> CGFloat {
        let key = (ids.count, columns, cardWidth)
        if rowsKey.map({ $0 != key }) ?? true {
            rows = computeRows(ids: ids, columns: max(columns, 1), cardWidth: cardWidth, input: input)
            rowsKey = key
        }
        let row = index / max(columns, 1)
        return row < rows.count ? rows[row] : TransferCardMetrics.height(titleLines: 1, chipLines: 0)
    }

    /// Recomputes the rows on next use (the torrents' order changed).
    func invalidateRows() {
        rowsKey = nil
    }

    private func computeRows(ids: [String], columns: Int, cardWidth: CGFloat, input: (Int) -> TransferCardLayoutInput) -> [CGFloat] {
        var result: [CGFloat] = []
        result.reserveCapacity(ids.count / columns + 1)
        var rowMax: CGFloat = 0
        for (index, id) in ids.enumerated() {
            if entries[id] == nil { update(id, input: input(index)) }
            let height = entries[id].map { Self.height($0, cardWidth: cardWidth) } ?? 0
            rowMax = max(rowMax, height)
            if index % columns == columns - 1 || index == ids.count - 1 {
                result.append(rowMax)
                rowMax = 0
            }
        }
        return result
    }

    private static func height(_ entry: Entry, cardWidth: CGFloat) -> CGFloat {
        let titleLines = TransferCardMetrics.titleLines(naturalWidth: entry.titleWidth, cardWidth: cardWidth)
        let chipLines: Int
        if entry.input.chips.isEmpty, entry.input.labels.isEmpty {
            chipLines = 0
        } else {
            chipLines = NameChipLayout(chips: entry.input.chips, labels: entry.input.labels, width: max(cardWidth - 2 * TransferCardMetrics.padding, 0),
                                       maxLines: TransferCardMetrics.chipLines).lineCount
        }
        return TransferCardMetrics.height(titleLines: titleLines, chipLines: chipLines)
    }
}
