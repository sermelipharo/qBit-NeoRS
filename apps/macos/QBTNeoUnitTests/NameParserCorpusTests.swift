// Test data: one name per line reads better than wrapped strings.
// swiftlint:disable line_length
import Foundation
import Testing

/// The built-in rules on realistic names: movies, series, anime with group
/// prefixes and checksums, music, software and games, Linux images and
/// Russian release names (more kinds of content in `contentCorpus`).
/// Expected values list the title, the content kind and every tag
/// ("category:text") in display order.
struct NameParserCorpusTests {
    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        let title: String
        let kind: ContentKind
        let chips: [String]

        var testDescription: String { name }
    }

    static let parser = NameParser(rules: NameRuleDefaults.rules)

    static let corpus: [Case] = [
        Case(name: "Movie.Name.2021.1080p.WEB-DL.DDP5.1.Atmos.H.264-FLUX.mkv",
             title: "Movie Name", kind: .video,
             chips: ["resolution:1080p", "source:WEB-DL", "videoCodec:H.264", "audio:DD+ 5.1", "audio:Atmos", "year:2021"]),
        Case(name: "Some.Movie.Title.2021.2160p.UHD.BluRay.REMUX.HDR.DV.TrueHD.7.1.Atmos.HEVC-GROUP.mkv",
             title: "Some Movie Title", kind: .video,
             chips: ["resolution:4K", "hdr:HDR", "hdr:Dolby Vision", "source:Remux", "videoCodec:HEVC", "audio:TrueHD 7.1", "audio:Atmos", "year:2021"]),
        Case(name: "The.Matrix.1999.REMASTERED.1080p.BluRay.x265.10bit.AAC.5.1-RARBG",
             title: "The Matrix", kind: .video,
             chips: ["resolution:1080p", "source:BluRay", "videoCodec:HEVC", "videoCodec:10-bit", "audio:AAC 5.1", "edition:REMASTERED", "year:1999", "releaseGroup:RARBG"]),
        Case(name: "Blade.Runner.2049.2017.2160p.UHD.BluRay.x265.HDR10Plus-TERMINAL",
             title: "Blade Runner 2049", kind: .video,
             chips: ["resolution:4K", "hdr:HDR10+", "source:BluRay", "videoCodec:HEVC", "year:2017", "releaseGroup:TERMINAL"]),
        Case(name: "1917.2019.1080p.BluRay.x264-SPARKS",
             title: "1917", kind: .video,
             chips: ["resolution:1080p", "source:BluRay", "videoCodec:H.264", "year:2019", "releaseGroup:SPARKS"]),
        Case(name: "Dune Part Two (2024) [2160p] [4K] [WEB] [5.1] [YTS.MX]",
             title: "Dune Part Two", kind: .video,
             chips: ["resolution:4K", "source:WEB", "audio:5.1", "year:2024", "releaseGroup:YTS.MX"]),
        Case(name: "Oppenheimer (2023) 1080p WEBRip x265 10bit DDP5.1-Tigole",
             title: "Oppenheimer", kind: .video,
             chips: ["resolution:1080p", "source:WEBRip", "videoCodec:HEVC", "videoCodec:10-bit", "audio:DD+ 5.1", "year:2023", "releaseGroup:Tigole"]),
        Case(name: "The.Lord.of.the.Rings.The.Fellowship.of.the.Ring.2001.EXTENDED.1080p.BluRay.x264-FSiHD",
             title: "The Lord of the Rings The Fellowship of the Ring", kind: .video,
             chips: ["resolution:1080p", "source:BluRay", "videoCodec:H.264", "edition:Extended", "year:2001", "releaseGroup:FSiHD"]),
        Case(name: "Apocalypse.Now.1979.Final.Cut.IMAX.Directors.Cut.720p.BluRay.DTS-HD.MA.5.1-GRP",
             title: "Apocalypse Now", kind: .video,
             chips: ["resolution:720p", "source:BluRay", "audio:DTS-HD MA 5.1", "edition:IMAX", "edition:Director's Cut", "year:1979", "releaseGroup:GRP"]),
        Case(name: "Mr. Robot (2015) Season 1 S01 1080p BluRay x265 HEVC 10bit AAC 5.1",
             title: "Mr. Robot", kind: .video,
             chips: ["seasonEpisode:S1", "resolution:1080p", "source:BluRay", "videoCodec:HEVC", "videoCodec:10-bit", "audio:AAC 5.1", "year:2015"]),
        Case(name: "Show.Name.S01E02.Episode.Title.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb",
             title: "Show Name", kind: .video,
             chips: ["seasonEpisode:S01E02", "resolution:1080p", "source:Amazon", "source:WEB-DL", "videoCodec:H.264", "audio:DD+ 5.1", "releaseGroup:NTb"]),
        Case(name: "Show Name S02E05E06 720p HDTV x264-KILLERS",
             title: "Show Name", kind: .video,
             chips: ["seasonEpisode:S02E05–E06", "resolution:720p", "source:HDTV", "videoCodec:H.264", "releaseGroup:KILLERS"]),
        Case(name: "Show.Name.S01E01-E03.1080p.NF.WEB-DL.x264",
             title: "Show Name", kind: .video,
             chips: ["seasonEpisode:S01E01–E03", "resolution:1080p", "source:Netflix", "source:WEB-DL", "videoCodec:H.264"]),
        Case(name: "The.Office.US.S01-S09.COMPLETE.720p.WEB-DL.x264",
             title: "The Office US", kind: .video,
             chips: ["seasonEpisode:S01–S09", "seasonEpisode:Complete", "resolution:720p", "source:WEB-DL", "videoCodec:H.264"]),
        Case(name: "Breaking Bad Season 1-5 Complete Series 1080p BluRay",
             title: "Breaking Bad", kind: .video,
             chips: ["seasonEpisode:S1–S5", "seasonEpisode:Complete", "resolution:1080p", "source:BluRay"]),
        Case(name: "Doctor.Who.2005.S13E01.1080p.iP.WEB-DL.AAC2.0.H.264-NTb",
             title: "Doctor Who", kind: .video,
             chips: ["seasonEpisode:S13E01", "resolution:1080p", "source:WEB-DL", "videoCodec:H.264", "audio:AAC 2.0", "year:2005", "releaseGroup:NTb"]),
        Case(name: "Spider-Man",
             title: "Spider-Man", kind: .other,
             chips: []),
        Case(name: "Spider-Man.No.Way.Home.2021.1080p.WEBRip.x264-RARBG",
             title: "Spider-Man No Way Home", kind: .video,
             chips: ["resolution:1080p", "source:WEBRip", "videoCodec:H.264", "year:2021", "releaseGroup:RARBG"]),
        Case(name: "[SubsPlease] Sousou no Frieren - 05 (1080p) [A1B2C3D4].mkv",
             title: "Sousou no Frieren", kind: .video,
             chips: ["seasonEpisode:E05", "resolution:1080p", "releaseGroup:SubsPlease"]),
        Case(name: "[Erai-raws] Jujutsu Kaisen - 24 [1080p][Multiple Subtitle][ABCD1234].mkv",
             title: "Jujutsu Kaisen", kind: .video,
             chips: ["seasonEpisode:E24", "resolution:1080p", "subtitles:MultiSub", "releaseGroup:Erai-raws"]),
        Case(name: "[Judas] Vinland Saga (Season 2) [1080p][HEVC x265 10bit][Eng-Subs]",
             title: "Vinland Saga", kind: .video,
             chips: ["seasonEpisode:S2", "resolution:1080p", "videoCodec:HEVC", "videoCodec:10-bit", "subtitles:ENG Sub", "releaseGroup:Judas"]),
        Case(name: "[HorribleSubs] One Piece - 1000 [720p].mkv",
             title: "One Piece", kind: .video,
             chips: ["seasonEpisode:E1000", "resolution:720p", "releaseGroup:HorribleSubs"]),
        Case(name: "[Commie] Steins;Gate - 01v2 [BD 1080p AAC] [5F22A7E1].mkv",
             title: "Steins;Gate", kind: .video,
             chips: ["seasonEpisode:E01", "resolution:1080p", "source:BluRay", "audio:AAC", "releaseGroup:Commie"]),
        Case(name: "[Nekomoe kissaten][Bocchi the Rock!][01-12][1080p][JPSC]",
             title: "Bocchi the Rock!", kind: .video,
             chips: ["seasonEpisode:E01–12", "resolution:1080p", "releaseGroup:Nekomoe kissaten", "releaseGroup:JPSC"]),
        Case(name: "Artist - Album (2020) [FLAC 24-96]",
             title: "Artist - Album", kind: .music,
             chips: ["audio:FLAC", "audio:24/96", "year:2020"]),
        Case(name: "Artist - Album Name (2019) MP3 320 kbps",
             title: "Artist - Album Name", kind: .music,
             chips: ["audio:MP3 320", "year:2019"]),
        Case(name: "Pink Floyd - The Dark Side of the Moon (1973) [24bit-192kHz] FLAC",
             title: "Pink Floyd - The Dark Side of the Moon", kind: .music,
             chips: ["audio:24/192", "audio:FLAC", "year:1973"]),
        Case(name: "Various Artists - Summer Hits 2023 (2023) MP3 [320]",
             title: "Various Artists - Summer Hits 2023", kind: .music,
             chips: ["audio:MP3", "audio:320 kbps", "year:2023"]),
        Case(name: "Adobe Photoshop 2024 v25.1.0.120 (x64) Multilingual Portable",
             title: "Adobe Photoshop", kind: .software,
             chips: ["audioLanguage:Multi", "platform:x64", "platform:Portable", "version:v25.1.0.120", "year:2024"]),
        Case(name: "Microsoft Office 2021 Pro Plus x64 RePack by KpoJIuK",
             title: "Microsoft Office", kind: .software,
             chips: ["platform:x64", "year:2021", "releaseGroup:KpoJIuK"]),
        Case(name: "Cyberpunk 2077 v2.1 [GOG] (2020) Multi12",
             title: "Cyberpunk 2077", kind: .game,
             chips: ["audioLanguage:Multi 12", "platform:GOG", "version:v2.1", "year:2020"]),
        Case(name: "Some Game (2023) RePack от xatab",
             title: "Some Game", kind: .game,
             chips: ["year:2023", "releaseGroup:xatab"]),
        Case(name: "ubuntu-24.04.2-desktop-amd64.iso",
             title: "ubuntu-24.04.2-desktop", kind: .software,
             chips: ["platform:x64"]),
        Case(name: "debian-12.9.0-amd64-DVD-1.iso",
             title: "debian-12.9.0", kind: .software,
             chips: ["source:DVD", "platform:x64"]),
        Case(name: "Fedora-Workstation-Live-x86_64-41",
             title: "Fedora-Workstation-Live", kind: .software,
             chips: ["platform:x64"]),
        Case(name: "archlinux-2025.01.01-x86_64.iso",
             title: "archlinux", kind: .software,
             chips: ["platform:x64", "other:2025.01.01"]),
        Case(name: "Wikipedia enwiki-20250101-pages-articles-multistream",
             title: "Wikipedia enwiki-20250101-pages-articles-multistream", kind: .other,
             chips: []),
        Case(name: "Project Gutenberg — Top 100 eBooks (EPUB)",
             title: "Project Gutenberg — Top 100 eBooks", kind: .book,
             chips: ["format:EPUB"]),
        Case(name: "Sintel (2010) [4K]",
             title: "Sintel", kind: .video,
             chips: ["resolution:4K", "year:2010"]),
        Case(name: "Big Buck Bunny (2008) [1080p]",
             title: "Big Buck Bunny", kind: .video,
             chips: ["resolution:1080p", "year:2008"]),
        Case(name: "Фильм (2021) WEB-DLRip 1080p | Дубляж",
             title: "Фильм", kind: .video,
             chips: ["resolution:1080p", "source:WEB-DLRip", "audioLanguage:Дубляж", "year:2021"]),
        Case(name: "Фильм / Film (2021) BDRip 720p от Group | D, P, A",
             title: "Фильм / Film", kind: .video,
             chips: ["resolution:720p", "source:BDRip", "year:2021", "releaseGroup:Group"]),
        Case(name: "Сериал / Series / Сезон: 1 / Серии: 1-8 из 10 (Режиссёр) [2021, WEB-DL 1080p] MVO",
             title: "Сериал / Series", kind: .video,
             chips: ["seasonEpisode:S1", "seasonEpisode:E1–8/10", "resolution:1080p", "source:WEB-DL", "audioLanguage:MVO", "year:2021"]),
        Case(name: "Мультфильм / Cartoon (2019) WEB-DL 2160p | HDR | Лицензия",
             title: "Мультфильм / Cartoon", kind: .video,
             chips: ["resolution:4K", "hdr:HDR", "source:WEB-DL", "year:2019", "other:Лицензия"]),
        Case(name: "Фильм.2020.BDRip.1080p.Rus.Eng.Sub",
             title: "Фильм", kind: .video,
             chips: ["resolution:1080p", "source:BDRip", "audioLanguage:RUS", "subtitles:ENG Sub", "year:2020"]),
        Case(name: "Сериал (2 сезон) 2022 WEB-DLRip | Многоголосый закадровый",
             title: "Сериал", kind: .video,
             chips: ["seasonEpisode:S2", "source:WEB-DLRip", "audioLanguage:MVO", "year:2022"]),
        Case(name: "www.Torrenting.com - Movie.Name.2022.720p.WEBRip.x264.AAC",
             title: "Movie Name", kind: .video,
             chips: ["resolution:720p", "source:WEBRip", "videoCodec:H.264", "audio:AAC", "year:2022"]),
        Case(name: "Movie.Name.2022.1080p.WEB.H264-EDITH[rarbg]",
             title: "Movie Name", kind: .video,
             chips: ["resolution:1080p", "source:WEB", "videoCodec:H.264", "year:2022", "releaseGroup:rarbg"]),
        Case(name: "Movie_Name_2020_1080p_BluRay_x264",
             title: "Movie Name", kind: .video,
             chips: ["resolution:1080p", "source:BluRay", "videoCodec:H.264", "year:2020"]),
        Case(name: "Cars.2.2011.1080p.BluRay.x264",
             title: "Cars 2", kind: .video,
             chips: ["resolution:1080p", "source:BluRay", "videoCodec:H.264", "year:2011"]),
    ]

    @Test func corpusIsLargeEnough() {
        #expect(Self.corpus.count >= 40)
        #expect(Self.contentCorpus.count >= 60)
        // Every kind of content is covered.
        #expect(Set((Self.corpus + Self.contentCorpus).map(\.kind)) == Set(ContentKind.allCases))
    }

    @Test(arguments: corpus + contentCorpus)
    func parsesName(_ sample: Case) {
        let parsed = Self.parser.parse(sample.name)
        #expect(parsed.title == sample.title)
        #expect(parsed.kind == sample.kind)
        #expect(parsed.chips.map { "\($0.category.rawValue):\($0.text)" } == sample.chips)
        // The title range points at the title's text in the original name.
        let raw = (sample.name as NSString).substring(with: parsed.titleRange)
        #expect(NameParser.cleanTitle(raw) == parsed.title)
    }
}
// swiftlint:enable line_length
