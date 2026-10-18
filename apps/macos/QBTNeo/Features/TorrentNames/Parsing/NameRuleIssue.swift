import Foundation

/// Why a name rule cannot be used.
enum NameRuleIssue: Error, Equatable, Sendable {
    case emptyPattern
    case patternTooLong(limit: Int)
    case invalidPattern
    case templateTooLong(limit: Int)
    /// The template uses `$n` but the pattern has fewer capture groups.
    case missingGroup(Int)
    /// Matching took too long on the test names (catastrophic backtracking).
    case tooSlow

    var message: String {
        switch self {
        case .emptyPattern:
            String(localized: "names.issue.empty", defaultValue: "Enter a regular expression.", comment: "Name rule validation")
        case .patternTooLong(let limit):
            String(localized: "names.issue.patternTooLong", defaultValue: "The pattern is longer than \(limit) characters.",
                   comment: "Name rule validation")
        case .invalidPattern:
            String(localized: "names.issue.invalid", defaultValue: "This isn't a valid regular expression.", comment: "Name rule validation")
        case .templateTooLong(let limit):
            String(localized: "names.issue.templateTooLong", defaultValue: "The tag text is longer than \(limit) characters.",
                   comment: "Name rule validation")
        case .missingGroup(let group):
            String(localized: "names.issue.missingGroup", defaultValue: "The tag text uses $\(group), but the pattern has no group \(group).",
                   comment: "Name rule validation")
        case .tooSlow:
            String(localized: "names.issue.tooSlow", defaultValue: "This pattern is too slow on long names. Simplify nested repetitions such as (a+)+.",
                   comment: "Name rule validation")
        }
    }
}
