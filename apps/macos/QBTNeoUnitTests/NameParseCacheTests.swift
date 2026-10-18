import Foundation
import Testing

struct NameParseCacheTests {
    private let rules = [NameRule(category: .resolution, pattern: "1080p")]

    @Test func cachesPerName() {
        let cache = NameParseCache(parser: NameParser(rules: rules))
        let first = cache.parsed("Film.1080p")
        #expect(cache.count == 1)
        #expect(cache.parsed("Film.1080p") == first)
        #expect(cache.count == 1)
        _ = cache.parsed("Other.1080p")
        #expect(cache.count == 2)
    }

    @Test func replacingTheParserInvalidates() {
        let cache = NameParseCache(parser: NameParser(rules: rules))
        #expect(cache.parsed("Film.1080p.x265").chips.count == 1)
        var more = rules
        more.append(NameRule(category: .videoCodec, pattern: "x265", template: "HEVC"))
        cache.replaceParser(NameParser(rules: more))
        #expect(cache.isEmpty)
        #expect(cache.parsed("Film.1080p.x265").chips.map(\.text) == ["1080p", "HEVC"])
    }

    @Test func optionsChangeTheResult() {
        let cache = NameParseCache(parser: NameParser(rules: NameRuleDefaults.rules))
        #expect(cache.parsed("Film.2020.1080p").title == "Film")
        cache.replaceParser(NameParser(rules: NameRuleDefaults.rules, options: .init(yearAsChip: false)))
        #expect(cache.parsed("Film.2020.1080p").title == "Film (2020)")
    }

    @Test func staysBounded() {
        let cache = NameParseCache(parser: NameParser(rules: rules), limit: 10)
        for index in 0 ..< 35 { _ = cache.parsed("Name \(index) 1080p") }
        #expect(cache.count <= 10)
        #expect(cache.parsed("Name 34 1080p").chips.count == 1)
    }
}
