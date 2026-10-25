#if DEBUG
import AppKit

/// Live-run scenario "live-sort-names" (use with `--demo`): sorting by name compares either the full names or the
/// parsed titles without a leading "The", in both directions.
extension UIScenarios {
    static func liveSortNames(_ d: UIDriver) async throws {
        try await waitRunning(d)
        let list = d.model.list
        let names = d.model.names
        list.sortColumn = .name
        list.sortAscending = true
        list.sortsNamesByTitle = false
        await d.pause(0.5)
        let full = list.visibleItems.map(\.name)
        d.log("full names: \(full)")
        d.check(full == full.sorted { $0.localizedStandardCompare($1) == .orderedAscending }, "full-name order is ascending")

        list.sortsNamesByTitle = true
        await d.pause(0.5)
        let keys = list.visibleItems.map { names.titleSortKey($0.name) ?? $0.name }
        d.log("title keys: \(keys)")
        d.check(keys == keys.sorted { $0.localizedStandardCompare($1) != .orderedDescending }, "title order is ascending")
        list.sortAscending = false
        await d.pause(0.5)
        let reversed = list.visibleItems.map { names.titleSortKey($0.name) ?? $0.name }
        d.check(reversed == keys.reversed() || reversed == reversed.sorted { $0.localizedStandardCompare($1) != .orderedAscending },
                "title order reverses")
        d.check(names.titleSortKey("The.Matrix.1999.1080p") == "Matrix", "a leading The is skipped")
        list.sortsNamesByTitle = false
        list.sortAscending = true
    }
}
#endif
