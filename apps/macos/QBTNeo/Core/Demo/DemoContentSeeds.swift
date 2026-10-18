import Foundation

/// Sample torrents of other kinds of content (software, music, podcasts,
/// games, ROM sets, audiobooks, books, comics, series, documentaries and
/// courses) with release-style names, so the cards view and the parsed
/// names show every content kind. All titles and people are invented.
extension DemoCatalog {
    static let contentCategories: [(name: String, path: String)] = [
        ("Series", ""),
        ("Games", ""),
        ("Emulation", ""),
        ("Audiobooks", ""),
        ("Comics", ""),
        ("Courses", ""),
    ]

    static let contentSeeds: [DemoSeed] = [
        DemoSeed(
            name: "Example.Show.2024.S02E05.1080p.WEB-DL.DDP5.1.H.264-GRP", gib: 2.4, state: .downloading, progress: 0.38,
            category: "Series", tags: ["favorite"], hosts: ["tracker.opentrackr.org"], addedDaysAgo: 0.1, downKiB: 5_400, upKiB: 320
        ),
        DemoSeed(
            name: "[SubsPlease] Example Anime - 05 (1080p) [A1B2C3D4].mkv", gib: 1.4, state: .uploading, progress: 1, category: "Series",
            hosts: ["tracker.opentrackr.org"], ratio: 2.4, addedDaysAgo: 3, upKiB: 410
        ),
        DemoSeed(
            name: "Сериал Пример / Example Series / Сезон: 1 / Серии: 1-10 из 10 (2023) WEB-DL 1080p | MVO", gib: 18.2, state: .stalledUploading,
            progress: 1, category: "Series", hosts: ["tracker.opentrackr.org"], ratio: 0.9, addedDaysAgo: 20
        ),
        DemoSeed(
            name: "Planet.Example.S01E03.Oceans.2160p.UHD.BluRay.DOCU.x265-GRP", gib: 9.6, state: .stoppedUploading, progress: 1,
            category: "Series", tags: ["4K"], hosts: ["tracker.opentrackr.org"], ratio: 1.6, addedDaysAgo: 40
        ),
        DemoSeed(
            name: "Udemy - Complete Example Programming Course (2023)", gib: 6.1, state: .downloading, progress: 0.71, category: "Courses",
            tags: ["work"], hosts: ["tracker.opentrackr.org"],
            layout: .files([("01 Introduction/01 Welcome.mp4", 0.02), ("01 Introduction/02 Setup.mp4", 0.04), ("02 Basics/01 Values.mp4", 0.3),
                            ("02 Basics/02 Functions.mp4", 0.3), ("Resources/slides.pdf", 0.04), ("03 Project/01 Project.mp4", 0.3)]),
            addedDaysAgo: 0.4, downKiB: 2_900, upKiB: 120
        ),
        DemoSeed(
            name: "Example Studio Pro 2025 v26.1.0.42 (x64) Multilingual Portable", gib: 3.1, state: .uploading, progress: 1, category: "Software",
            hosts: ["tracker.opentrackr.org"], ratio: 1.3, addedDaysAgo: 7, upKiB: 240
        ),
        DemoSeed(
            name: "Notes App 2.4.0 (arm64) [macOS 14+]", gib: 0.12, state: .stalledUploading, progress: 1, category: "Software",
            hosts: ["tracker.opentrackr.org"], ratio: 4.2, addedDaysAgo: 16
        ),
        DemoSeed(
            name: "Example Artist - Night Drive (2023) [FLAC 24bit-96kHz] {Vinyl Rip}", gib: 1.9, state: .downloading, progress: 0.52,
            category: "Music", tags: ["favorite"], hosts: ["tracker.opentrackr.org"],
            layout: .files([("A1 Night Drive.flac", 0.2), ("A2 Neon.flac", 0.2), ("B1 Coastline.flac", 0.25), ("B2 Morning.flac", 0.3), ("cover.jpg", 0.05)]),
            addedDaysAgo: 0.3, downKiB: 3_100, upKiB: 150
        ),
        DemoSeed(
            name: "The Placeholders - Northern Lights EP (2021) [WEB FLAC]", gib: 0.3, state: .uploading, progress: 1, category: "Music",
            hosts: ["tracker.opentrackr.org"], ratio: 3.1, addedDaysAgo: 30, upKiB: 90
        ),
        DemoSeed(
            name: "Podcast Example Show - Episode 120 (2024) [MP3 128 kbps]", gib: 0.06, state: .stalledUploading, progress: 1, category: "Music",
            hosts: ["tracker.opentrackr.org"], ratio: 0.6, addedDaysAgo: 2
        ),
        DemoSeed(
            name: "Space Example Deluxe Edition v2.3.1 (GOG) MULTi10", gib: 14.8, state: .downloading, progress: 0.24, category: "Games",
            hosts: ["tracker.opentrackr.org"], addedDaysAgo: 0.2, downKiB: 8_700, upKiB: 510
        ),
        DemoSeed(
            name: "Example Platformer (World) (Homebrew) [SNES]", gib: 0.004, state: .uploading, progress: 1, category: "Emulation",
            hosts: ["tracker.opentrackr.org"], ratio: 9.5, addedDaysAgo: 60, upKiB: 12
        ),
        DemoSeed(
            name: "MAME 0.262 ROMs (Merged)", gib: 38.0, state: .stoppedDownloading, progress: 0.12, category: "Emulation",
            tags: ["archive"], hosts: ["tracker.opentrackr.org"], addedDaysAgo: 5
        ),
        DemoSeed(
            name: "Петров Пётр - Тихий город (читает Сергей Примеров) [2021, MP3, 128 kbps]", gib: 0.55, state: .uploading, progress: 1,
            category: "Audiobooks", hosts: ["tracker.opentrackr.org"], ratio: 1.1, addedDaysAgo: 11, upKiB: 60
        ),
        DemoSeed(
            name: "Jane Example - The Quiet Harbor (Unabridged) [M4B] Narrated by John Placeholder", gib: 0.42, state: .stalledDownloading,
            progress: 0.66, category: "Audiobooks", hosts: ["tracker.opentrackr.org"], addedDaysAgo: 1
        ),
        DemoSeed(
            name: "Example Author - Collected Stories (2020) [EPUB, FB2, PDF]", gib: 0.02, state: .uploading, progress: 1, category: "Books",
            hosts: ["tracker.opentrackr.org"], ratio: 5.0, addedDaysAgo: 90, upKiB: 8
        ),
        DemoSeed(
            name: "[ScanGroup] Example Manga v01-v12 (Digital) [CBZ]", gib: 2.2, state: .queuedDownloading, progress: 0.08, category: "Comics",
            hosts: ["tracker.opentrackr.org"], addedDaysAgo: 0.6, queuePosition: 3
        ),
        DemoSeed(
            name: "Example Hero 001 (2023) (Digital) (Zone-Empire).cbr", gib: 0.05, state: .stalledUploading, progress: 1, category: "Comics",
            hosts: ["tracker.opentrackr.org"], ratio: 2.2, addedDaysAgo: 25
        ),
    ]

    // MARK: Names for the 2,000-torrent stress data set

    /// Name patterns of other kinds; `#` is replaced by a number.
    static let largeNamePatterns = [
        "Example.Show.#.S01E0#.1080p.WEB-DL.x265-GRP", "Example Artist # - Album (2019) [FLAC 24-96]", "Example Author # - Stories [EPUB, FB2]",
        "Example Game # v1.#.2 (GOG) MULTi8", "Example Tool # 2.#.1 (x64) Portable", "[Group] Example Anime # - 0# (1080p).mkv",
        "Example Manga # v0#-v12 (Digital) [CBZ]", "Example Author # - Novel (Unabridged) [M4B]",
    ]
}
