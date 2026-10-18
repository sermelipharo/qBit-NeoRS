import Foundation

/// Parse results per torrent name. Names change rarely, so each is parsed
/// once; replacing the parser (rules or options changed) empties the cache.
/// Bounded: when full, it starts over (the names on screen are parsed again
/// on their next draw, which costs well under a millisecond each).
final class NameParseCache {
    static let defaultLimit = 4_096

    private(set) var parser: NameParser
    private var entries: [String: ParsedName] = [:]
    private let limit: Int

    init(parser: NameParser, limit: Int = defaultLimit) {
        self.parser = parser
        self.limit = max(limit, 1)
    }

    var count: Int { entries.count }
    var isEmpty: Bool { entries.isEmpty }

    func parsed(_ name: String) -> ParsedName {
        if let cached = entries[name] { return cached }
        let result = parser.parse(name)
        if entries.count >= limit { entries.removeAll(keepingCapacity: true) }
        entries[name] = result
        return result
    }

    func replaceParser(_ parser: NameParser) {
        self.parser = parser
        entries.removeAll()
    }
}
