import Foundation

/// Checks name rules before they are used. `NSRegularExpression` has no
/// timeout, so a rule is also run against long test names with a time
/// budget (matching reports progress and stops when the budget is spent);
/// a pattern that backtracks catastrophically is rejected when saved or
/// imported, and the parser keeps its own per-name budget as a backstop.
enum NameRuleValidator {
    static let maxPatternLength = 300
    static let maxTemplateLength = 80
    /// Time allowed for one rule over all test names.
    static let timeBudget = Duration.milliseconds(60)

    /// Compiles a rule's pattern as written. Token boundaries (letters and
    /// digits never touch a match) are checked by the parser, not added to the
    /// expression: ICU keeps a copy of `\p{L}`-style sets per expression,
    /// which cost about 40 KB per rule.
    static func compile(_ pattern: String, caseSensitive: Bool) -> NSRegularExpression? {
        let options: NSRegularExpression.Options = caseSensitive ? [] : [.caseInsensitive]
        return try? NSRegularExpression(pattern: pattern, options: options)
    }

    /// Cheap checks for live feedback while typing (no test run).
    static func quickIssue(_ rule: NameRule) -> NameRuleIssue? {
        if case .failure(let issue) = checked(rule) { return issue }
        return nil
    }

    /// The compiled expression of a rule that passes the cheap checks.
    static func checked(_ rule: NameRule) -> Result<NSRegularExpression, NameRuleIssue> {
        let pattern = rule.pattern
        if pattern.trimmingCharacters(in: .whitespaces).isEmpty { return .failure(.emptyPattern) }
        if pattern.utf16.count > maxPatternLength { return .failure(.patternTooLong(limit: maxPatternLength)) }
        if rule.template.utf16.count > maxTemplateLength { return .failure(.templateTooLong(limit: maxTemplateLength)) }
        guard let regex = compile(pattern, caseSensitive: rule.isCaseSensitive) else { return .failure(.invalidPattern) }
        if let group = highestGroup(in: rule.template), group > regex.numberOfCaptureGroups { return .failure(.missingGroup(group)) }
        return .success(regex)
    }

    /// Every check, including the timed run (when saving or importing).
    static func issue(_ rule: NameRule, budget: Duration = timeBudget) -> NameRuleIssue? {
        let regex: NSRegularExpression
        switch checked(rule) {
        case .success(let compiled): regex = compiled
        case .failure(let issue): return issue
        }
        let clock = ContinuousClock()
        let deadline = clock.now.advanced(by: budget)
        for sample in samples {
            var exceeded = false
            regex.enumerateMatches(in: sample, options: [.reportProgress], range: NSRange(location: 0, length: sample.utf16.count)) { _, _, stop in
                if clock.now > deadline {
                    exceeded = true
                    stop.pointee = true
                }
            }
            if exceeded || clock.now > deadline { return .tooSlow }
        }
        return nil
    }

    /// The largest `$n` the template refers to (`\$` is a literal dollar sign).
    static func highestGroup(in template: String) -> Int? {
        var highest: Int?
        var characters = template.makeIterator()
        var pending: Character? = characters.next()
        while let character = pending {
            pending = characters.next()
            if character == "\\" {
                pending = characters.next()
                continue
            }
            guard character == "$" else { continue }
            var digits = ""
            while let next = pending, next.isASCII, next.isNumber {
                digits.append(next)
                pending = characters.next()
            }
            if let group = Int(digits) { highest = max(highest ?? group, group) }
        }
        return highest
    }

    /// Realistic names plus long inputs that make nested repetitions explode,
    /// all at the parser's input limit.
    private static let samples: [String] = {
        let limit = NameParser.maxInputLength
        let realistic = [
            "Some.Movie.Title.2021.2160p.UHD.BluRay.REMUX.HDR.DV.TrueHD.7.1.Atmos.HEVC-GROUP.mkv",
            "[SubsPlease] Some Anime Title - 05 (1080p) [A1B2C3D4].mkv",
            "Фильм / Film (2021) WEB-DLRip 1080p | Дубляж, MVO",
        ]
        let long = ["a", "1", "a.", "a-", "a ", "ab1_", "[a]", ".", "\u{0430}"].map { unit in
            String(String(repeating: unit, count: limit / unit.utf16.count).prefix(limit - 1)) + "!"
        }
        return realistic + long
    }()
}
