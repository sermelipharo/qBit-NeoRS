import Foundation

/// Splits a torrent name into a clean title and technical tags with an
/// ordered list of rules. Pure and thread-safe; results are cached per name
/// by `NameParseCache`, never computed in a view body.
///
/// Rules run in order. Each match claims its text: the text is masked before
/// the next rule runs, so later rules cannot match across it and earlier rules
/// win ("ENG Sub" is subtitles, not an English audio track). Then:
/// * the technical tokens at the start ("[Group] ") are skipped, and the title
///   is the text from there to the first technical token after it;
/// * of several years before the first other token, only the last one is a
///   year ("Blade.Runner.2049.2017…"), the others stay in the title;
/// * a release group at the end ("…-GROUP") counts only after another token,
///   so "Spider-Man" keeps its "Man";
/// * the title's separators are normalized (dots and underscores become
///   spaces unless the title already has spaces; dots between digits stay).
struct NameParser: Sendable {
    struct Options: Equatable, Sendable {
        /// Off: the year is appended to the title instead ("Title (2021)").
        var yearAsChip = true
    }

    /// Longer names are only searched in their first part (UTF-16 units).
    static let maxInputLength = 512
    /// Backstop for one name: later rules are skipped once it is spent
    /// (validated rules take microseconds each).
    static let timeBudget = Duration.milliseconds(25)

    let options: Options
    private let rules: [CompiledNameRule]

    init(rules: [NameRule], options: Options = Options()) {
        self.rules = rules.compactMap { $0.isEnabled ? CompiledNameRule($0) : nil }
        self.options = options
    }

    /// Number of rules in use (enabled and valid).
    var activeRuleCount: Int { rules.count }

    private struct Token {
        let range: NSRange
        let category: NameRuleCategory
        let text: String

        var end: Int { range.location + range.length }
    }

    func parse(_ name: String) -> ParsedName {
        let full = name as NSString
        let length = min(full.length, Self.maxInputLength)
        guard length > 0, !rules.isEmpty else { return .plain(name, kind: ContentKindDetector.shared.kind(name: name, chips: [])) }
        let source = length < full.length ? full.substring(to: length) : name
        var tokens = match(source, length: length)
        guard !tokens.isEmpty else { return .plain(name, kind: ContentKindDetector.shared.kind(name: name, chips: [])) }
        tokens.sort { $0.range.location < $1.range.location }

        let nsSource = source as NSString
        let titleStart = leadingEnd(of: tokens, in: nsSource)
        tokens = disambiguateYears(tokens, from: titleStart)
        tokens = dropLoneTrailingGroups(tokens, from: titleStart, in: nsSource)

        let titleEnd = tokens.first { $0.range.location >= titleStart }?.range.location ?? full.length
        var titleRange = Self.trimmed(NSRange(location: titleStart, length: max(titleEnd - titleStart, 0)), in: full)
        if !Self.hasWordCharacters(full.substring(with: titleRange)) {
            titleRange = fallbackTitleRange(tokens, in: full)
        }
        var title = Self.cleanTitle(full.substring(with: titleRange))

        var chips = Self.chips(from: tokens)
        let kind = ContentKindDetector.shared.kind(name: name, chips: chips)
        if !options.yearAsChip, let year = chips.first(where: { $0.category == .year }) {
            chips.removeAll { $0.category == .year }
            if !title.hasSuffix(year.text) { title += " (\(year.text))" }
        }
        return ParsedName(title: title, titleRange: titleRange, chips: chips, technicalRanges: tokens.map(\.range), kind: kind)
    }

    // MARK: Matching

    private func match(_ source: String, length: Int) -> [Token] {
        let work = NSMutableString(string: source)
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: Self.timeBudget)
        let whole = NSRange(location: 0, length: length)
        var tokens: [Token] = []
        var claimed: [NSRange] = []
        // Refreshed only after a rule claimed text (most rules match nothing).
        var text = work.copy() as? String ?? source
        for rule in rules {
            guard clock.now < deadline else { break }
            var found: [NSRange] = []
            // No `.reportProgress`: it makes matching about 4x slower. Rules
            // are timed by `NameRuleValidator` before they are used.
            let units = text as NSString
            rule.regex.enumerateMatches(in: text, options: [], range: whole) { result, _, _ in
                guard let result, result.range.length > 0, Self.isWholeToken(result.range, in: units),
                      !claimed.contains(where: { NSIntersectionRange($0, result.range).length > 0 })
                else { return }
                let raw = rule.template.isEmpty
                    ? (source as NSString).substring(with: result.range)
                    : rule.regex.replacementString(for: result, in: source, offset: 0, template: rule.template)
                tokens.append(Token(range: result.range, category: rule.category, text: Self.normalizedTag(raw)))
                found.append(result.range)
            }
            for range in found {
                claimed.append(range)
                // Masked text separates tokens like punctuation and matches nothing.
                work.replaceCharacters(in: range, with: String(repeating: "\u{1}", count: range.length))
            }
            if !found.isEmpty { text = work.copy() as? String ?? text }
        }
        return tokens
    }

    /// Letters and digits never touch a match: tokens are whole words whatever
    /// the separator (dots, underscores, spaces, dashes, brackets, masked text).
    /// A match that starts or ends with punctuation brings its own boundary.
    static func isWholeToken(_ range: NSRange, in text: NSString) -> Bool {
        let end = range.location + range.length
        if range.location > 0, isWordUnit(text.character(at: range.location - 1)), isWordUnit(text.character(at: range.location)) {
            return false
        }
        if end < text.length, isWordUnit(text.character(at: end)), isWordUnit(text.character(at: end - 1)) {
            return false
        }
        return true
    }

    /// Letters and digits; surrogate halves (emoji, rare scripts) count as letters.
    private static func isWordUnit(_ unit: unichar) -> Bool {
        guard let scalar = Unicode.Scalar(unit) else { return true }
        return CharacterSet.alphanumerics.contains(scalar)
    }

    // MARK: Title

    /// End of the technical tokens at the very start ("[Group] ", site prefixes).
    private func leadingEnd(of tokens: [Token], in source: NSString) -> Int {
        var end = 0
        for token in tokens {
            guard token.range.location >= end else { continue }
            let gap = source.substring(with: NSRange(location: end, length: token.range.location - end))
            guard Self.isJunk(gap) else { break }
            end = token.end
        }
        return end
    }

    /// Keeps only the last year before the first other token; earlier ones are part of the title.
    private func disambiguateYears(_ tokens: [Token], from start: Int) -> [Token] {
        let firstOther = tokens.first { $0.range.location >= start && $0.category != .year }?.range.location ?? Int.max
        let early = tokens.filter { $0.category == .year && $0.range.location >= start && $0.range.location < firstOther }
        guard early.count > 1, let kept = early.last else { return tokens }
        let dropped = Set(early.map(\.range.location)).subtracting([kept.range.location])
        return tokens.filter { !($0.category == .year && dropped.contains($0.range.location)) }
    }

    /// A release group after the title ("-GROUP", "[Group]", "от Group") needs another tag before it.
    private func dropLoneTrailingGroups(_ tokens: [Token], from start: Int, in source: NSString) -> [Token] {
        tokens.filter { token in
            guard token.category == .releaseGroup, token.range.location >= start, token.range.location > 0,
                  ["-", "[", "о", "О"].contains(source.substring(with: NSRange(location: token.range.location, length: 1)))
            else { return true }
            return tokens.contains {
                $0.range.location < token.range.location && $0.category != .releaseGroup && $0.category != .hidden
            }
        }
    }

    /// The first stretch between tokens that has letters or digits, else the whole name.
    private func fallbackTitleRange(_ tokens: [Token], in name: NSString) -> NSRange {
        var cursor = 0
        for token in tokens + [Token(range: NSRange(location: name.length, length: 0), category: .hidden, text: "")] {
            if token.range.location > cursor {
                let range = Self.trimmed(NSRange(location: cursor, length: token.range.location - cursor), in: name)
                if Self.hasWordCharacters(name.substring(with: range)) { return range }
            }
            cursor = max(cursor, token.end)
        }
        return NSRange(location: 0, length: name.length)
    }

    private static let separators = CharacterSet(charactersIn: " \t._-–—|/\\:;,+~#*\u{1}")
    private static let junk = separators.union(CharacterSet(charactersIn: "[](){}<>"))
    private static let leadingTrim = separators.union(CharacterSet(charactersIn: ")]}>"))
    private static let trailingTrim = separators.union(CharacterSet(charactersIn: "([{<"))

    static func isJunk(_ text: String) -> Bool {
        text.unicodeScalars.allSatisfy { junk.contains($0) }
    }

    static func hasWordCharacters(_ text: String) -> Bool {
        text.unicodeScalars.contains { CharacterSet.alphanumerics.contains($0) }
    }

    /// `range` without separators and dangling brackets at either end, and
    /// without brackets around all of it ("[Title]").
    static func trimmed(_ range: NSRange, in name: NSString) -> NSRange {
        var start = range.location
        var end = range.location + range.length
        while true {
            while start < end, let scalar = Unicode.Scalar(name.character(at: start)), leadingTrim.contains(scalar) { start += 1 }
            while end > start, let scalar = Unicode.Scalar(name.character(at: end - 1)), trailingTrim.contains(scalar) { end -= 1 }
            guard end - start >= 2, let open = Unicode.Scalar(name.character(at: start)),
                  let close = Unicode.Scalar(name.character(at: end - 1)), pairs[open] == close else { break }
            let inner = name.substring(with: NSRange(location: start + 1, length: end - start - 2))
            guard !inner.unicodeScalars.contains(where: { $0 == open || $0 == close }) else { break }
            start += 1
            end -= 1
        }
        return NSRange(location: start, length: end - start)
    }

    private static let pairs: [Unicode.Scalar: Unicode.Scalar] = ["[": "]", "(": ")", "{": "}"]

    /// Dots and underscores as word separators become spaces; runs of spaces collapse.
    static func cleanTitle(_ raw: String) -> String {
        let characters = Array(raw.replacing("_", with: " "))
        let dotsSeparate = !characters.contains(" ")
        var result = ""
        for (index, character) in characters.enumerated() {
            if character == ".", dotsSeparate {
                let betweenDigits = index > 0 && index + 1 < characters.count
                    && characters[index - 1].isNumber && characters[index + 1].isNumber
                result.append(betweenDigits ? "." : " ")
            } else {
                result.append(character)
            }
        }
        return result.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    // MARK: Tags

    /// Case and leading zeros don't make a tag different ("S1" and "S01").
    static func dedupKey(_ text: String) -> String {
        var key = ""
        var previousIsDigit = false
        var pendingZeros = ""
        for character in text.lowercased() {
            if character == "0", !previousIsDigit {
                pendingZeros.append(character)
                continue
            }
            if character.isNumber {
                pendingZeros = ""
                previousIsDigit = true
            } else {
                key += pendingZeros
                pendingZeros = ""
                previousIsDigit = false
            }
            key.append(character)
        }
        return key + pendingZeros
    }

    static func normalizedTag(_ raw: String) -> String {
        raw.split(whereSeparator: \.isWhitespace).joined(separator: " ")
    }

    /// Ordered by category, then position; the same text in a category appears once.
    private static func chips(from tokens: [Token]) -> [NameChip] {
        let candidates = tokens.enumerated()
            .filter { $0.element.category.makesChip && !$0.element.text.isEmpty }
            .sorted { ($0.element.category.order, $0.offset) < ($1.element.category.order, $1.offset) }
        var seen = Set<String>()
        var chips: [NameChip] = []
        for (_, token) in candidates where seen.insert(token.category.rawValue + "\u{0}" + dedupKey(token.text)).inserted {
            chips.append(NameChip(category: token.category, text: token.text))
        }
        return chips
    }
}
