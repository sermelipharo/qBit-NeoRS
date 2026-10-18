import CoreGraphics

/// Where the transfer list was scrolled when its table went away (the main
/// window released its views while hidden), so the rebuilt table opens at
/// the same rows: the torrent at the top edge and how far it was scrolled past.
struct TransferScrollPosition: Sendable, Equatable {
    var topID: String
    /// Distance from the top of that row to the top of the visible area.
    var offset: CGFloat
    var x: CGFloat
}
