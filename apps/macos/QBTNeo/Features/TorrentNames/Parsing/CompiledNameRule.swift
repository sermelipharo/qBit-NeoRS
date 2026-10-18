import Foundation

/// A name rule with its regular expression compiled once.
struct CompiledNameRule: @unchecked Sendable {
    // `NSRegularExpression` is immutable and documented as thread-safe.
    let regex: NSRegularExpression
    let category: NameRuleCategory
    let template: String

    init?(_ rule: NameRule) {
        guard case .success(let regex) = NameRuleValidator.checked(rule) else { return nil }
        self.regex = regex
        category = rule.category
        template = rule.template
    }
}
