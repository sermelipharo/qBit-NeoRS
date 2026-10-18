/// A window that can show a sheet (`NSWindow`; test doubles in the unit tests).
@MainActor
protocol SheetHosting: AnyObject {
    /// The sheet attached to this window, if any.
    var hostedSheet: (any SheetHosting)? { get }
    /// Ends `sheet`, which is attached to this window.
    func endHostedSheet(_ sheet: any SheetHosting)
}
