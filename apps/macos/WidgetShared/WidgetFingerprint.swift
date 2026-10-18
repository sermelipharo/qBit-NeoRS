import Foundation

/// Hashes of a snapshot's content in two parts: what changes rarely and
/// matters (status, counts, which torrents, their states) and what changes
/// all the time (speeds bucketed to half-octaves, whole progress percents).
/// Hash values are only compared within one process.
struct WidgetFingerprint: Hashable, Sendable {
    var structural: Int
    var volatile: Int

    /// Speed bucket: 0 when idle, then one step per factor of about 1.41, so
    /// jitter around a steady speed does not count as a change.
    static func bucket(_ rate: Int64) -> Int {
        rate <= 0 ? 0 : 1 + Int((log2(Double(rate)) * 2).rounded(.down))
    }
}
