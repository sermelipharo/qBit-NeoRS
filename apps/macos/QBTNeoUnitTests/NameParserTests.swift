import Foundation
import Testing

struct NameParserTests {
    private func rule(_ category: NameRuleCategory, _ pattern: String, _ template: String = "", caseSensitive: Bool = false) -> NameRule {
        NameRule(category: category, pattern: pattern, template: template, isCaseSensitive: caseSensitive)
    }

    @Test func noRulesLeavesTheNameAlone() {
        let parsed = NameParser(rules: []).parse("Some.Name.1080p")
        #expect(parsed == .plain("Some.Name.1080p"))
    }

    @Test func emptyNameIsPlain() {
        #expect(NameParser(rules: NameRuleDefaults.rules).parse("") == .plain(""))
    }

    @Test func templatesUseCaptureGroups() {
        let parser = NameParser(rules: [rule(.resolution, "2160p|4K|UHD", "4K"), rule(.seasonEpisode, #"S(\d+)E(\d+)"#, "Season $1, episode $2")])
        let parsed = parser.parse("Show.S02E07.UHD")
        #expect(parsed.chips == [NameChip(category: .seasonEpisode, text: "Season 02, episode 07"), NameChip(category: .resolution, text: "4K")])
        #expect(parsed.title == "Show")
    }

    @Test func tokensAreWholeWords() {
        let parser = NameParser(rules: [rule(.resolution, "720p")])
        #expect(parser.parse("Name.1720p").chips.isEmpty)
        #expect(parser.parse("Name_720p_x").chips.count == 1)
        #expect(parser.parse("Name [720p]").chips.count == 1)
    }

    @Test func earlierRulesClaimTheirText() {
        // "ENG Sub" is subtitles; the audio language rule cannot reuse its "ENG".
        let parser = NameParser(rules: [rule(.subtitles, #"ENG[ ._-]?Sub"#, "ENG Sub"), rule(.audioLanguage, "ENG", "ENG")])
        #expect(parser.parse("Film.2020.ENG.Sub").chips == [NameChip(category: .subtitles, text: "ENG Sub")])
        #expect(parser.parse("Film.2020.ENG").chips == [NameChip(category: .audioLanguage, text: "ENG")])
    }

    @Test func maskedTextSeparatesLaterMatches() {
        // "UHD" is claimed first; "BluRay Remux" still matches on its own.
        let parser = NameParser(rules: [rule(.resolution, "UHD", "4K"), rule(.source, #"(?:UHD[ ._-]?)?(?:Blu-?Ray)?[ ._-]?Remux"#, "Remux")])
        #expect(parser.parse("Film.UHD.BluRay.Remux").chips.map(\.text) == ["4K", "Remux"])
    }

    @Test func disabledRulesAreIgnored() {
        var off = rule(.resolution, "1080p")
        off.isEnabled = false
        #expect(NameParser(rules: [off]).parse("Film.1080p").chips.isEmpty)
        #expect(NameParser(rules: [off]).activeRuleCount == 0)
    }

    @Test func invalidRulesAreSkipped() {
        let parser = NameParser(rules: [rule(.other, "(unclosed"), rule(.resolution, "1080p")])
        #expect(parser.activeRuleCount == 1)
        #expect(parser.parse("Film.1080p").chips.map(\.text) == ["1080p"])
    }

    @Test func caseSensitivity() {
        let sensitive = NameParser(rules: [rule(.source, "WEB", caseSensitive: true)])
        #expect(sensitive.parse("The.Web.2020").chips.isEmpty)
        #expect(sensitive.parse("Film.WEB").chips.count == 1)
        #expect(NameParser(rules: [rule(.source, "WEB")]).parse("The.Web").chips.count == 1)
    }

    @Test func yearCanStayInTheTitle() {
        let parser = NameParser(rules: NameRuleDefaults.rules, options: .init(yearAsChip: false))
        let parsed = parser.parse("Movie.Name.2021.1080p.WEB-DL")
        #expect(parsed.title == "Movie Name (2021)")
        #expect(!parsed.chips.contains { $0.category == .year })
    }

    @Test func duplicatesCollapse() {
        let parser = NameParser(rules: NameRuleDefaults.rules)
        let chips = parser.parse("Show Season 1 S01 x265 HEVC").chips
        #expect(chips.filter { $0.category == .seasonEpisode }.count == 1)
        #expect(chips.filter { $0.category == .videoCodec }.count == 1)
    }

    @Test func titleRangeCoversOnlyTheTitle() {
        let name = "[Group] Some Title - 05 (1080p).mkv"
        let parsed = NameParser(rules: NameRuleDefaults.rules).parse(name)
        #expect((name as NSString).substring(with: parsed.titleRange) == "Some Title")
        #expect(!parsed.technicalRanges.isEmpty)
        #expect(parsed.technicalRanges.allSatisfy { NSIntersectionRange($0, parsed.titleRange).length == 0 })
    }

    @Test func longNamesAreOnlySearchedUpToTheLimit() {
        let name = String(repeating: "Long Title ", count: 80) + "1080p"
        let parsed = NameParser(rules: NameRuleDefaults.rules).parse(name)
        #expect(parsed.chips.isEmpty)
        #expect(parsed.title.hasPrefix("Long Title Long Title"))
    }

    @Test func cleanTitleRules() {
        #expect(NameParser.cleanTitle("Movie.Name") == "Movie Name")
        #expect(NameParser.cleanTitle("Mr. Robot") == "Mr. Robot")
        #expect(NameParser.cleanTitle("ubuntu-24.04.2-desktop") == "ubuntu-24.04.2-desktop")
        #expect(NameParser.cleanTitle("Movie_Name  Part_2") == "Movie Name Part 2")
    }

    @Test func dedupKeyIgnoresCaseAndLeadingZeros() {
        #expect(NameParser.dedupKey("S01") == NameParser.dedupKey("s1"))
        #expect(NameParser.dedupKey("S10") != NameParser.dedupKey("S1"))
        #expect(NameParser.dedupKey("AAC 2.0") == "aac 2.0")
    }

    @Test func parsingIsFast() {
        let parser = NameParser(rules: NameRuleDefaults.rules)
        let names = NameParserCorpusTests.corpus.map(\.name)
        let clock = ContinuousClock()
        let elapsed = clock.measure {
            for _ in 0 ..< 5 { for name in names { _ = parser.parse(name) } }
        }
        // About 0.15 ms per name in a release build; generous for debug builds and loaded machines.
        #expect(elapsed < .milliseconds(names.count * 5 * 5))
    }
}
