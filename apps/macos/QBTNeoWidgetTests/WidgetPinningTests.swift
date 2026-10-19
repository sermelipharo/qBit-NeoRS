import Foundation
import Testing

struct WidgetPinningTests {
    private let hash = "0123456789abcdef0123456789abcdef01234567"

    @Test func pinnedIDsAreValidDistinctAndBounded() {
        let many = (0..<30).map { String(format: "%040x", $0) }
        let pinned = WidgetTorrentSelector.pinnedIDs([hash, "NOT-HEX", "", hash] + many)
        #expect(pinned.first == hash)
        #expect(pinned.count == WidgetTorrentSelector.maxPinned)
        #expect(Set(pinned).count == pinned.count)
        #expect(!pinned.contains("NOT-HEX"))
    }

    @Test func candidatesPutTransfersFirstThenRecent() {
        func torrent(_ id: String, down: Int64, added: Int64) -> WidgetTorrent {
            WidgetTorrent(id: id, name: id, state: down > 0 ? .downloading : .stopped, progress: 0.5, downloadRate: down, uploadRate: 0,
                          eta: -1, size: 0, category: "", addedOn: added, completedOn: 0)
        }
        let torrents = [torrent("old", down: 0, added: 1), torrent("fast", down: 9, added: 0), torrent("new", down: 0, added: 5)]
        #expect(WidgetTorrentSelector.candidates(torrents).map { torrents[$0].id } == ["fast", "new", "old"])
        #expect(WidgetTorrentSelector.candidates(TestTorrents.make(count: 500, seed: 4)).count == WidgetTorrentSelector.maxCandidates)
    }

    @Test func searchMatchesEveryWordIgnoringCaseAndDiacritics() {
        let candidates = [WidgetTorrentRef(id: "a", name: "Ubuntu 24.04 Desktop"), WidgetTorrentRef(id: "b", name: "Pokémon Soundtrack"),
                          WidgetTorrentRef(id: "c", name: "Debian Desktop")]
        #expect(WidgetTorrentSelector.search(candidates, for: "desktop UBUNTU").map(\.id) == ["a"])
        #expect(WidgetTorrentSelector.search(candidates, for: "pokemon").map(\.id) == ["b"])
        #expect(WidgetTorrentSelector.search(candidates, for: "  ").count == 3)
        #expect(WidgetTorrentSelector.search(candidates, for: "arch").isEmpty)
    }

    @Test func detailFieldsAreBounded() {
        let torrent = WidgetTorrent(id: hash, name: "x", state: .downloading, progress: 0.5, downloadRate: 0, uploadRate: 0, eta: -1,
                                    size: 1, category: "", addedOn: 0, completedOn: 0)
        let detail = WidgetTorrentDetail(
            torrent: torrent, downloaded: -1, ratio: .infinity, seeds: 1, seedsInSwarm: -1, peers: 0, peersInSwarm: -1, availability: .nan,
            tags: Array(repeating: String(repeating: "t", count: 99), count: 9), savePath: "/Users/me/Downloads/Linux ISOs/",
            errorMessage: String(repeating: "e", count: 999),
            history: (0..<200).map { WidgetSpeedSample(time: Int64($0), download: 1, upload: 1) }
        )
        #expect(detail.downloaded == 0)
        #expect(detail.ratio == 0)
        #expect(detail.availability == -1)
        #expect(detail.tags.count == WidgetTorrentDetail.maxTags)
        #expect(detail.tags.allSatisfy { $0.count == WidgetTorrentDetail.maxTagLength })
        #expect(detail.location == "Downloads/Linux ISOs")
        #expect(detail.errorMessage.count == WidgetTorrentDetail.maxErrorLength)
        #expect(detail.history.count == WidgetSpeedHistory.capacity)
        #expect(detail.history.last?.time == 199)
    }

    @Test func nameTitleAndChipsAreBoundedAndOptional() throws {
        let torrent = WidgetTorrent(id: hash, name: "Big.Buck.Bunny.2008.1080p", state: .seeding, progress: 1, downloadRate: 0,
                                    uploadRate: 0, eta: -1, size: 1, category: "", addedOn: 0, completedOn: 0)
        func detail(title: String?, chips: [WidgetNameChip]) -> WidgetTorrentDetail {
            WidgetTorrentDetail(torrent: torrent, downloaded: 1, ratio: 1, seeds: 0, seedsInSwarm: -1, peers: 0, peersInSwarm: -1,
                                availability: -1, tags: [], savePath: "", errorMessage: "", history: [], title: title, chips: chips)
        }
        let chip = WidgetNameChip(text: String(repeating: "c", count: 99), tint: .blue, isOutlined: false)
        let split = detail(title: "Big Buck Bunny", chips: Array(repeating: chip, count: 20))
        #expect(split.displayTitle == "Big Buck Bunny")
        #expect(split.chips?.count == WidgetTorrentDetail.maxChips)
        #expect(chip.text.count == WidgetNameChip.maxLength)
        let whole = detail(title: "", chips: [])
        #expect(whole.displayTitle == torrent.name)
        #expect(whole.chips == nil)
        // A snapshot written before titles and chips existed still decodes.
        var json = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(split)) as? [String: Any])
        json["n"] = nil
        json["c"] = nil
        let old = try JSONDecoder().decode(WidgetTorrentDetail.self, from: JSONSerialization.data(withJSONObject: json))
        #expect(old.displayTitle == torrent.name)
        #expect(old.chips == nil)
    }

    @Test func textHelpers() {
        #expect(WidgetText.pathTail("/a/b/c/") == "b/c")
        #expect(WidgetText.pathTail("") == "")
        #expect(WidgetText.bounded("abcdef", 4) == "abc…")
        #expect(WidgetText.bounded("abcdef", 4, keepingEnd: true) == "…def")
        #expect(WidgetText.isTorrentID(hash))
        #expect(!WidgetText.isTorrentID(hash.uppercased()))
        #expect(!WidgetText.isTorrentID("../etc"))
        #expect(!WidgetText.isTorrentID(String(repeating: "a", count: 65)))
    }

    @Test func linksRoundTripAndRejectEverythingElse() throws {
        let url = try #require(WidgetLink.torrent(hash))
        #expect(url.absoluteString == "qbitneors://torrent/\(hash)")
        #expect(WidgetLink.torrentID(from: url) == hash)
        #expect(WidgetLink.torrent("../x") == nil)
        for bad in ["qbitneors://torrent/zz", "qbitneors://torrent/\(hash)/x", "qbitneors://settings/\(hash)", "magnet:?xt=urn:btih:\(hash)",
                    "https://torrent/\(hash)", "qbitneors://torrent/%2e%2e"] {
            #expect(WidgetLink.torrentID(from: try #require(URL(string: bad))) == nil, "\(bad)")
        }
    }

    @Test func pinnedTorrentsCountInTheFingerprint() {
        var snapshot = WidgetSnapshot(generatedAt: .now, status: .running)
        let base = snapshot.fingerprint
        snapshot.candidates = [WidgetTorrentRef(id: hash, name: "n")]
        #expect(snapshot.fingerprint == base)
        snapshot.removed = [hash]
        #expect(snapshot.fingerprint.structural != base.structural)
    }
}
