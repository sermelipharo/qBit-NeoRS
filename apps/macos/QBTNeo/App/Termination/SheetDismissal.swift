/// Closes every open sheet before quitting (`TerminationGate`): the innermost
/// first, so a sheet shown on a sheet (an alert over a dialog) ends before
/// its parent. Bounded, in case a sheet refuses to end.
@MainActor
enum SheetDismissal {
    static func hasSheets(_ windows: [any SheetHosting]) -> Bool {
        windows.contains { $0.hostedSheet != nil }
    }

    /// Ends the sheets of `windows`; returns how many were ended.
    @discardableResult
    static func closeAll(_ windows: [any SheetHosting], limit: Int = 32) -> Int {
        var ended = 0
        for window in windows {
            while ended < limit, let sheet = window.hostedSheet {
                // Walk down to the innermost sheet and end it from its parent.
                var parent = window
                var innermost = sheet
                while let next = innermost.hostedSheet {
                    parent = innermost
                    innermost = next
                }
                parent.endHostedSheet(innermost)
                ended += 1
                // A sheet that refuses to end stops this window's loop.
                if parent.hostedSheet === innermost { break }
            }
        }
        return ended
    }
}
