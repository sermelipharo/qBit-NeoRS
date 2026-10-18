import Foundation

/// Deterministic pseudo-random torrents for the selector tests.
struct TestTorrents {
    /// SplitMix64: a tiny seeded generator, so failures reproduce.
    struct Generator: RandomNumberGenerator {
        var state: UInt64

        mutating func next() -> UInt64 {
            state &+= 0x9E37_79B9_7F4A_7C15
            var value = state
            value = (value ^ (value >> 30)) &* 0xBF58_476D_1CE4_E5B9
            value = (value ^ (value >> 27)) &* 0x94D0_49BB_1331_11EB
            return value ^ (value >> 31)
        }
    }

    static let categories = ["", "Linux", "Movies", "Music", "Books"]

    static func make(count: Int, seed: UInt64) -> [WidgetTorrent] {
        var random = Generator(state: seed)
        return (0..<count).map { index in
            let state = WidgetTorrentState.allCases.randomElement(using: &random) ?? .stopped
            // Few distinct values, so ties are common and the tie-breaker matters.
            let rates: [Int64] = [0, 0, 0, 1_000, 50_000, 50_000, 2_000_000]
            let complete = state.isSeedingState || state == .completed
            return WidgetTorrent(
                id: String(format: "%040x", index), name: "Torrent \(index)", state: state,
                progress: complete ? 1 : Double(Int.random(in: 0...100, using: &random)) / 100,
                downloadRate: complete ? 0 : rates.randomElement(using: &random) ?? 0,
                uploadRate: rates.randomElement(using: &random) ?? 0, eta: Int64.random(in: -1...10_000, using: &random),
                size: 1_000_000, category: categories.randomElement(using: &random) ?? "",
                addedOn: Int64.random(in: 1_000...1_010, using: &random), completedOn: complete ? Int64.random(in: 1_000...1_020, using: &random) : 0
            )
        }
    }
}
