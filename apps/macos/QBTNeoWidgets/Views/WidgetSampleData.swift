import Foundation

/// Realistic made-up data for the widget gallery, the placeholder and previews.
enum WidgetSampleData {
    static func snapshot(now: Date = .now, altSpeed: Bool = false) -> WidgetSnapshot {
        let time = Int64(now.timeIntervalSince1970)
        var snapshot = WidgetSnapshot(generatedAt: now, status: .running)
        snapshot.downloadRate = 14_890_000
        snapshot.uploadRate = 1_240_000
        snapshot.altSpeedEnabled = altSpeed
        snapshot.counts = WidgetCounts(total: 24, active: 5, downloading: 4, seeding: 15)
        snapshot.history = (0..<WidgetSpeedHistory.capacity).map { index in
            let phase = Double(index)
            let download = 9_000_000 + 4_500_000 * sin(phase / 6) + 2_000_000 * sin(phase / 2.3) + 120_000 * phase
            let upload = 1_000_000 + 350_000 * sin(phase / 4 + 1) + 150_000 * cos(phase / 1.7)
            let at = time - Int64(WidgetSpeedHistory.capacity - 1 - index) * WidgetSpeedHistory.interval
            return WidgetSpeedSample(time: at, download: Int64(max(download, 0)), upload: Int64(max(upload, 0)))
        }
        snapshot.torrents = [
            torrent("ubuntu-24.04.1-desktop-amd64.iso", .downloading, 0.62, down: 8_650_000, up: 310_000, eta: 252,
                    category: "Linux ISOs", added: time - 900),
            torrent("Fedora-Workstation-Live-x86_64-41", .downloading, 0.31, down: 4_420_000, up: 120_000, eta: 1_140,
                    category: "Linux ISOs", added: time - 1_600),
            torrent("Blender Studio – Wing It! (4K)", .downloading, 0.88, down: 1_820_000, up: 64_000, eta: 95,
                    category: "Open Movies", added: time - 3_000),
            torrent("debian-12.8.0-arm64-netinst.iso", .seeding, 1, down: 0, up: 520_000, eta: 0,
                    category: "Linux ISOs", added: time - 86_400, completed: time - 80_000),
            torrent("archlinux-2024.12.01-x86_64.iso", .metadata, 0, down: 0, up: 0, eta: -1,
                    category: "Linux ISOs", added: time - 60),
            torrent("Big Buck Bunny (1080p)", .completed, 1, down: 0, up: 0, eta: 0,
                    category: "Open Movies", added: time - 7_200, completed: time - 400),
            torrent("Sintel – Director's Cut", .stalledSeeding, 1, down: 0, up: 0, eta: 0,
                    category: "Open Movies", added: time - 172_800, completed: time - 170_000),
        ]
        snapshot.categories = [
            WidgetCategory(name: "Linux ISOs", counts: WidgetCounts(total: 14, active: 4, downloading: 3, seeding: 9)),
            WidgetCategory(name: "Open Movies", counts: WidgetCounts(total: 10, active: 1, downloading: 1, seeding: 6)),
        ]
        return snapshot
    }

    /// The sample torrent a Torrent widget follows in the gallery.
    static let pinnedID = "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678"

    /// A Torrent widget entry following the sample torrent in `state`.
    static func torrentEntry(_ state: WidgetTorrentState, now: Date = .now) -> SnapshotEntry {
        var snapshot = snapshot(now: now)
        snapshot.pinned = [pinnedDetail(state, now: now)]
        snapshot.candidates = snapshot.torrents.map { WidgetTorrentRef(id: $0.id, name: $0.name) }
        return SnapshotEntry(date: now, snapshot: snapshot, pinnedID: pinnedID, pinnedName: "Big.Buck.Bunny.2008.1080p.BluRay.x264.AAC-BLENDER")
    }

    static func pinnedDetail(_ state: WidgetTorrentState, now: Date = .now) -> WidgetTorrentDetail {
        let time = Int64(now.timeIntervalSince1970)
        let complete = state == .completed || state.isSeedingState
        let progress = complete ? 1 : (state.isProblem ? 0.47 : 0.62)
        let down: Int64 = state == .downloading ? 8_650_000 : 0
        let up: Int64 = state == .seeding ? 2_340_000 : (state == .downloading ? 310_000 : 0)
        let size: Int64 = 6_203_355_136
        let torrent = WidgetTorrent(
            id: pinnedID, name: "Big.Buck.Bunny.2008.1080p.BluRay.x264.AAC-BLENDER", state: state, progress: progress, downloadRate: down, uploadRate: up,
            eta: state == .downloading ? 252 : -1, size: size, category: "Movies", addedOn: time - 900,
            completedOn: complete ? time - 3_600 : 0
        )
        let history = (0..<WidgetSpeedHistory.capacity).map { index in
            let phase = Double(index)
            let at = time - Int64(WidgetSpeedHistory.capacity - 1 - index) * WidgetSpeedHistory.interval
            let wave = 1 + 0.35 * sin(phase / 5) + 0.15 * sin(phase / 1.9)
            return WidgetSpeedSample(time: at, download: state == .downloading ? Int64(7_000_000 * wave) : 0,
                                     upload: Int64(Double(max(up, 200_000)) * (2 - wave)))
        }
        return WidgetTorrentDetail(
            torrent: torrent, downloaded: Int64(Double(size) * progress), ratio: complete ? 1.84 : 0.07, seeds: 38, seedsInSwarm: 412,
            peers: 6, peersInSwarm: 57, availability: 38.91, tags: ["open movies"], savePath: "/Users/me/Movies/Blender",
            errorMessage: state.isProblem ? String(localized: "The files were moved or deleted.") : "", history: history,
            title: "Big Buck Bunny", chips: [
                WidgetNameChip(text: "1080p", tint: .blue, isOutlined: false),
                WidgetNameChip(text: "BluRay", tint: .purple, isOutlined: false),
                WidgetNameChip(text: "x264", tint: .teal, isOutlined: false),
                WidgetNameChip(text: "AAC", tint: .indigo, isOutlined: false),
                WidgetNameChip(text: "2008", tint: .gray, isOutlined: false),
                WidgetNameChip(text: "BLENDER", tint: .gray, isOutlined: true)
            ]
        )
    }

    static func entry(filter: WidgetTorrentFilter = .active, category: String? = nil, now: Date = .now) -> SnapshotEntry {
        SnapshotEntry(date: now, snapshot: snapshot(now: now), filter: filter, category: category)
    }

    /// The name doubles as the identifier (the first 16 characters are unique here).
    private static func torrent(_ name: String, _ state: WidgetTorrentState, _ progress: Double, down: Int64, up: Int64,
                                eta: Int64, category: String, added: Int64, completed: Int64 = 0) -> WidgetTorrent {
        WidgetTorrent(id: name, name: name, state: state, progress: progress, downloadRate: down, uploadRate: up, eta: eta,
                      size: 4_700_000_000, category: category, addedOn: added, completedOn: completed)
    }
}
