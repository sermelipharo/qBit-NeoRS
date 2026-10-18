import Foundation
import Testing

struct WidgetSnapshotTests {
    /// The largest snapshot the app can publish.
    private func fullSnapshot() -> WidgetSnapshot {
        var snapshot = WidgetSnapshot(generatedAt: Date(timeIntervalSince1970: 1_800_000_000), status: .running)
        snapshot.downloadRate = 123_456_789
        snapshot.uploadRate = 9_876_543
        snapshot.counts = WidgetCounts(total: 5_000, active: 300, downloading: 200, seeding: 4_000)
        var history = WidgetSpeedHistory()
        for step in 0..<200 {
            history.record(download: 99_999_999, upload: 99_999_999, at: 1_800_000_000 + Int64(step) * WidgetSpeedHistory.interval)
        }
        snapshot.history = history.samples
        snapshot.torrents = (0..<WidgetTorrentSelector.maxPublished).map { index in
            WidgetTorrent(id: String(repeating: "f", count: 64), name: String(repeating: "Ж", count: 300), state: .downloading,
                          progress: 0.123_456_789, downloadRate: 99_999_999, uploadRate: 99_999_999, eta: 86_399, size: 999_999_999_999,
                          category: String(repeating: "c", count: 40), addedOn: 1_800_000_000 + Int64(index), completedOn: 1_800_000_000)
        }
        snapshot.categories = (0..<WidgetTorrentSelector.maxCategories).map {
            WidgetCategory(name: "Category \($0)", counts: WidgetCounts(total: 100, active: 10, downloading: 5, seeding: 50))
        }
        snapshot.pinned = (0..<WidgetTorrentSelector.maxPinned).map { index in
            WidgetTorrentDetail(
                torrent: snapshot.torrents[index], downloaded: 999_999_999_999, ratio: 123.456, seeds: 9_999, seedsInSwarm: 99_999,
                peers: 9_999, peersInSwarm: 99_999, availability: 999.999, tags: Array(repeating: String(repeating: "т", count: 100), count: 20),
                savePath: String(repeating: "/папка", count: 100), errorMessage: String(repeating: "ошибка ", count: 100), history: history.samples
            )
        }
        snapshot.removed = (0..<WidgetTorrentSelector.maxPinned).map { _ in String(repeating: "e", count: 64) }
        snapshot.candidates = (0..<WidgetTorrentSelector.maxCandidates).map { _ in
            WidgetTorrentRef(id: String(repeating: "d", count: 64), name: String(repeating: "Ж", count: 300))
        }
        return snapshot
    }

    @Test func roundTrips() throws {
        let snapshot = fullSnapshot()
        let decoded = WidgetSnapshot.decode(try snapshot.encoded())
        #expect(decoded == snapshot)
    }

    @Test func staysSmall() throws {
        let size = try fullSnapshot().encoded().count
        #expect(size < 128 * 1_024, "worst case is \(size) bytes")
        #expect(size < WidgetSnapshot.maxEncodedSize)
    }

    @Test func rejectsOtherVersionsAndGarbage() throws {
        var snapshot = fullSnapshot()
        snapshot.version = WidgetSnapshot.currentVersion + 1
        #expect(WidgetSnapshot.decode(try snapshot.encoded()) == nil)
        #expect(WidgetSnapshot.decode(Data("{\"v\":1}".utf8)) == nil)
        #expect(WidgetSnapshot.decode(Data(repeating: 0x20, count: WidgetSnapshot.maxEncodedSize + 1)) == nil)
    }

    @Test func torrentFieldsAreBoundedAndClamped() {
        let torrent = WidgetTorrent(id: String(repeating: "a", count: 100), name: String(repeating: "x", count: 500), state: .downloading,
                                    progress: .nan, downloadRate: -5, uploadRate: 3, eta: 120, size: 1, category: "", addedOn: 0, completedOn: 0)
        #expect(torrent.id.count == WidgetTorrent.idLength)
        #expect(torrent.name.count == WidgetTorrent.maxNameLength)
        #expect(torrent.name.hasSuffix("…"))
        #expect(torrent.progress == 0)
        #expect(torrent.downloadRate == 0)
        #expect(torrent.remainingTime == 120)
    }

    @Test func remainingTimeIsNilWhenUnknown() {
        func eta(_ seconds: Int64, progress: Double = 0.5) -> Int64? {
            WidgetTorrent(id: "a", name: "a", state: .downloading, progress: progress, downloadRate: 0, uploadRate: 0, eta: seconds, size: 0,
                          category: "", addedOn: 0, completedOn: 0).remainingTime
        }
        #expect(eta(-1) == nil)
        #expect(eta(WidgetTorrent.infiniteETA) == nil)
        #expect(eta(60, progress: 1) == nil)
    }

    @Test func countsPerCategory() {
        var snapshot = fullSnapshot()
        snapshot.categories = [WidgetCategory(name: "Linux", counts: WidgetCounts(total: 3, active: 1))]
        #expect(snapshot.counts(category: nil) == snapshot.counts)
        #expect(snapshot.counts(category: "Linux").total == 3)
        #expect(snapshot.counts(category: "Missing") == WidgetCounts())
    }

    @Test func countsClassifyTorrents() {
        var counts = WidgetCounts()
        for torrent in TestTorrents.make(count: 200, seed: 3) { counts.add(torrent) }
        let torrents = TestTorrents.make(count: 200, seed: 3)
        #expect(counts.total == 200)
        #expect(counts.active == torrents.filter(\.isTransferring).count)
        #expect(counts.seeding == torrents.filter(\.state.isSeedingState).count)
        #expect(counts.count(for: .downloading) == torrents.filter(\.isDownloading).count)
    }

    @Test func fingerprintIgnoresTimeAndHistoryButNotState() {
        let snapshot = fullSnapshot()
        var later = snapshot
        later.generatedAt = later.generatedAt.addingTimeInterval(60)
        later.history.removeAll()
        #expect(later.fingerprint == snapshot.fingerprint)

        var jitter = snapshot
        jitter.downloadRate += 1_000
        #expect(jitter.fingerprint == snapshot.fingerprint)

        var faster = snapshot
        faster.downloadRate *= 2
        #expect(faster.fingerprint.structural == snapshot.fingerprint.structural)
        #expect(faster.fingerprint.volatile != snapshot.fingerprint.volatile)

        var alt = snapshot
        alt.altSpeedEnabled = true
        #expect(alt.fingerprint.structural != snapshot.fingerprint.structural)
    }

    @Test func speedBuckets() {
        #expect(WidgetFingerprint.bucket(0) == 0)
        #expect(WidgetFingerprint.bucket(-3) == 0)
        #expect(WidgetFingerprint.bucket(1) == 1)
        #expect(WidgetFingerprint.bucket(1_100_000) == WidgetFingerprint.bucket(1_200_000))
        #expect(WidgetFingerprint.bucket(1_100_000) != WidgetFingerprint.bucket(1_700_000))
    }

    @Test func historyKeepsOneSamplePerIntervalAndTheNewest() {
        var history = WidgetSpeedHistory()
        let first = history.record(download: 1, upload: 1, at: 100)
        let tooSoon = history.record(download: 2, upload: 2, at: 100 + WidgetSpeedHistory.interval - 1)
        let next = history.record(download: -5, upload: 3, at: 100 + WidgetSpeedHistory.interval)
        #expect(first && !tooSoon && next)
        #expect(history.samples.last?.download == 0)
        for step in 2...200 { history.record(download: Int64(step), upload: 0, at: 100 + Int64(step) * WidgetSpeedHistory.interval) }
        #expect(history.samples.count == WidgetSpeedHistory.capacity)
        #expect(history.samples.last?.download == 200)
        // A clock that went backwards starts sampling again.
        let afterClockChange = history.record(download: 1, upload: 1, at: 0)
        #expect(afterClockChange)
    }
}
