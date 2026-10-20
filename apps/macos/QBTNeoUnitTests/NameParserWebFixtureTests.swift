import Foundation
import Testing

/// The Web UI parses names with a TypeScript port of `NameParser`; this test
/// keeps `crates/qbt-core/webui/test/names-fixture.json` (the rules plus what
/// this parser makes of the corpus and of extra names) in step, and
/// `scripts/test-webui.sh` checks that the port gives identical results.
/// After changing the parser, the built-in rules or the corpus, regenerate it:
///
///     TEST_RUNNER_QBT_WRITE_NAME_FIXTURE=1 xcodebuild test -project apps/macos/QBTNeo.xcodeproj \
///         -scheme QBTNeo -only-testing:QBTNeoUnitTests/NameParserWebFixtureTests
struct NameParserWebFixtureTests {
    struct Rule: Codable, Equatable {
        let pattern: String
        let category: String
        let template: String
        let caseSensitive: Bool
    }

    struct Result: Codable, Equatable {
        let name: String
        let yearAsChip: Bool
        let title: String
        /// UTF-16 start and end of the title in the name.
        let titleRange: [Int]
        /// [category, text] in display order.
        let chips: [[String]]
        let kind: String
    }

    struct RuleSet: Codable, Equatable {
        let rules: [Rule]
        let results: [Result]
    }

    struct Fixture: Codable, Equatable {
        var format = "qbitneors.name-parser-fixture"
        /// The built-in rules over the corpus and extra names.
        let builtIn: RuleSet
        /// Hand-written rules that exercise templates, flags and escapes.
        let custom: RuleSet
        /// [category name, kind or ""].
        let categoryKinds: [[String]]
    }

    static let fixtureURL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .appending(path: "crates/qbt-core/webui/test/names-fixture.json")

    /// Edge cases beyond the corpus: separators, brackets, scripts, long input.
    static let extraNames: [String] = [
        "", " ", "...", "[]", "Some.Name.1080p", "Name.1720p", "Name_720p_x", "Name [720p]", "Film.UHD.BluRay.Remux",
        "Show Season 1 S01 x265 HEVC", "[Group] Some Title - 05 (1080p).mkv", "Movie.Name.2021.1080p.WEB-DL",
        String(repeating: "Long Title ", count: 80) + "1080p", String(repeating: "Ä", count: 600) + ".mkv",
        "Spider-Man", "Spider-Man.2002.1080p", "holiday.mkv", "Some Song.flac", "notes.txt", "Romance Novels (EPUB)",
        "Episodes", "xXx.2002", "Something XXX", "Фильм (2020) WEB-DLRip 1080p | Дубляж", "Сериал / Series [S01] (2019) WEBRip от Group",
        "😀 Emoji Title 2019 1080p", "Title\u{301}d.2010.720p", "(2019)", "2019", "[SubsPlease] Title - 01 [1080p] [ABCDEF12].mkv",
        "Title_With_Underscores_2004_DVDRip", "A.B.C.1.2.3", "Name (USA) (Rev 1).zip", "Book Title - Author (2020) [EPUB]",
        "Band - Album (2001) [FLAC 24-96]", "Course.Name.Udemy.2023", "Tool v1.2 (x64) [macOS]", "  spaced   out  name  ",
        "name.with.tab\there", "[[Nested] Brackets]", "{Braces} Title 1999", "Title -GROUP", "Title.2020-GROUP",
    ]

    static let customRules: [NameRule] = [
        NameRule(category: .hidden, pattern: #"\[[0-9A-F]{8}\]"#),
        NameRule(category: .seasonEpisode, pattern: #"S(\d+)E(\d+)"#, template: "Season $1, episode $2"),
        NameRule(category: .resolution, pattern: "2160p|4K|UHD", template: "4K"),
        NameRule(category: .subtitles, pattern: #"ENG[ ._-]?Sub"#, template: "ENG Sub"),
        NameRule(category: .audioLanguage, pattern: "ENG", template: "ENG"),
        NameRule(category: .source, pattern: "WEB", isCaseSensitive: true),
        NameRule(category: .other, pattern: #"(?i)foo(?-i:BAR)"#, isCaseSensitive: true),
        NameRule(category: .other, pattern: #"x(?i)ab|cd"#, isCaseSensitive: true),
        NameRule(category: .edition, pattern: #"\bDirector\w*\b"#),
        NameRule(category: .format, pattern: #"\QC++\E"#),
        NameRule(category: .source, pattern: #"\p{Cyrillic}{3,}\-Rip"#),
        NameRule(category: .year, pattern: #"(?<=\()\d{4}(?=\))"#),
        NameRule(category: .other, pattern: #"\x{263A}"#, template: "smile"),
        NameRule(category: .other, pattern: #"Ü\w+"#),
        NameRule(category: .volume, pattern: #"(?i:vol)\.?\s?(\d+)"#, template: "Vol $1", isCaseSensitive: true),
        NameRule(category: .other, pattern: #"[a-c]{2}\d"#),
        NameRule(category: .platform, pattern: #"[^\s\d]{2}\d{2}x"#),
        NameRule(category: .other, pattern: #"PRICE(\d+)"#, template: #"\$$1"#),
        NameRule(category: .version, pattern: #"T(\d)"#, template: "$12"),
        NameRule(category: .region, pattern: #"\(((?:USA|Europe)(?:, ?[A-Z][a-z]+)*)\)"#, template: "$1"),
        NameRule(category: .releaseGroup, pattern: #"-([A-Za-z]+)$"#, template: "$1"),
        NameRule(category: .other, pattern: "(unclosed"),
        NameRule(isEnabled: false, category: .other, pattern: "Title"),
    ]

    static let customNames: [String] = [
        "Show.S02E07.UHD", "Film.2020.ENG.Sub", "Film.2020.ENG", "The.Web.2020", "Film.WEB", "fooBAR.FOOBAR.fooBar",
        "xAB cd CD", "Directors Cut", "C++ Primer", "Фильм ДВДРип-Rip", "Name (2021) x", "smile ☺ here", "Über Ülrich ülrich",
        "Vol.3 VOL 4 vol5", "AB1 abc1", "ab12x Ж_12x", "PRICE12 T5", "Game (USA, Europe) [ABCDEF12]", "Name-GROUP",
        "Title [DEADBEEF] -Group", "show s01e01",
    ]

    static let categoryNames = [
        "Movies/Blender Open Movies", "Linux ISOs/Debian-based", "Audiobooks", "Books", "Романы", "Romance", "ROMs", "Музыка",
        "Сериалы", "Datasets", "", "TV", "tv shows", "Apps", "XXX", "Emulation", "Игры", "ebooks", "Comics & Manga",
    ]

    static func rule(_ rule: NameRule) -> Rule {
        Rule(pattern: rule.pattern, category: rule.category.rawValue, template: rule.template, caseSensitive: rule.isCaseSensitive)
    }

    static func results(_ parser: NameParser, _ names: [String], yearAsChip: Bool) -> [Result] {
        names.map { name in
            let parsed = parser.parse(name)
            return Result(name: name, yearAsChip: yearAsChip, title: parsed.title,
                          titleRange: [parsed.titleRange.location, parsed.titleRange.location + parsed.titleRange.length],
                          chips: parsed.chips.map { [$0.category.rawValue, $0.text] }, kind: parsed.kind.rawValue)
        }
    }

    static func makeFixture() -> Fixture {
        let corpus = (NameParserCorpusTests.corpus + NameParserCorpusTests.contentCorpus).map(\.name) + extraNames
        let builtIn = NameRuleDefaults.rules.filter(\.isEnabled)
        let withYear = NameParser(rules: builtIn)
        let withoutYear = NameParser(rules: builtIn, options: .init(yearAsChip: false))
        let custom = NameParser(rules: customRules)
        return Fixture(
            builtIn: RuleSet(rules: builtIn.map(rule), results: results(withYear, corpus, yearAsChip: true) + results(withoutYear, corpus, yearAsChip: false)),
            // The Web UI only gets enabled rules; invalid ones are skipped by both parsers.
            custom: RuleSet(rules: customRules.filter(\.isEnabled).map(rule), results: results(custom, customNames, yearAsChip: true)),
            categoryKinds: categoryNames.map { [$0, ContentKindDetector.kind(categoryName: $0)?.rawValue ?? ""] }
        )
    }

    @Test func fixtureIsCurrent() throws {
        let fixture = Self.makeFixture()
        if ProcessInfo.processInfo.environment["QBT_WRITE_NAME_FIXTURE"] == "1" {
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
            try FileManager.default.createDirectory(at: Self.fixtureURL.deletingLastPathComponent(), withIntermediateDirectories: true)
            try (encoder.encode(fixture) + Data("\n".utf8)).write(to: Self.fixtureURL, options: .atomic)
        }
        let stored = try JSONDecoder().decode(Fixture.self, from: Data(contentsOf: Self.fixtureURL))
        #expect(stored == fixture, "names-fixture.json is out of date; regenerate it (see NameParserWebFixtureTests)")
    }

    @Test func webCompatibility() {
        for rule in NameRuleDefaults.rules {
            #expect(NameRuleWebCompatibility.unsupportedConstruct(in: rule.pattern) == nil, "\(rule.id)")
        }
        for rule in Self.customRules {
            #expect(NameRuleWebCompatibility.unsupportedConstruct(in: rule.pattern) == nil, "\(rule.pattern)")
        }
        let unsupported = [
            ("(?>ab)c", "(?>"), ("a*+b", "*+"), ("a++", "++"), ("a?+", "?+"), ("a{2}+", "}+"), (#"\Gx"#, #"\G"#), (#"\X"#, #"\X"#),
            ("[a[b]]", "[[…]]"), ("[a-z&&[^b]]", "&&"), ("[:alpha:]+", "[:…:]"), ("(?x) a", "(?x"), ("(?s:.)", "(?s"),
            (#"\p{InCyrillic}"#, #"\p{In…}"#), (#"[\W]"#, #"[\W]"#),
        ]
        for (pattern, construct) in unsupported {
            #expect(NameRuleWebCompatibility.unsupportedConstruct(in: pattern) == construct, "\(pattern)")
        }
        for pattern in [#"\Q(?>\E"#, #"\++"#, "[+]+", "(?i:a)", "(?-i)b", "(?<=x)y", "(?<name>a)", "[:x]", #"\p{Latin}"#] {
            #expect(NameRuleWebCompatibility.unsupportedConstruct(in: pattern) == nil, "\(pattern)")
        }
    }
}
