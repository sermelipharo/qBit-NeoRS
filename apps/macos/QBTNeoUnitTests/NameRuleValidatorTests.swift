import Foundation
import Testing

struct NameRuleValidatorTests {
    private func rule(_ pattern: String, _ template: String = "") -> NameRule {
        NameRule(category: .other, pattern: pattern, template: template)
    }

    @Test func everyBuiltInRuleIsValidAndFast() {
        for rule in NameRuleDefaults.rules {
            #expect(NameRuleValidator.issue(rule) == nil, "\(rule.id)")
        }
    }

    @Test func builtInRulesHaveUniqueIDsAndLocalizedNames() {
        let ids = NameRuleDefaults.rules.map(\.id)
        #expect(Set(ids).count == ids.count)
        for rule in NameRuleDefaults.rules {
            #expect(NameRuleDefaults.localizedName(of: rule.id) != nil, "\(rule.id)")
            #expect(!rule.displayName.isEmpty, "\(rule.id)")
        }
    }

    @Test func emptyAndOverlongPatterns() {
        #expect(NameRuleValidator.quickIssue(rule("  ")) == .emptyPattern)
        let long = String(repeating: "a", count: NameRuleValidator.maxPatternLength + 1)
        #expect(NameRuleValidator.quickIssue(rule(long)) == .patternTooLong(limit: NameRuleValidator.maxPatternLength))
        let template = String(repeating: "t", count: NameRuleValidator.maxTemplateLength + 1)
        #expect(NameRuleValidator.quickIssue(rule("x", template)) == .templateTooLong(limit: NameRuleValidator.maxTemplateLength))
    }

    @Test func invalidPattern() {
        #expect(NameRuleValidator.quickIssue(rule("(unclosed")) == .invalidPattern)
        #expect(NameRuleValidator.quickIssue(rule("[a-")) == .invalidPattern)
    }

    @Test func templateGroupsMustExist() {
        #expect(NameRuleValidator.quickIssue(rule(#"S(\d+)"#, "S$1")) == nil)
        #expect(NameRuleValidator.quickIssue(rule(#"S(\d+)"#, "S$2")) == .missingGroup(2))
        #expect(NameRuleValidator.quickIssue(rule("x", "$0")) == nil)
        #expect(NameRuleValidator.quickIssue(rule("x", #"costs \$5"#)) == nil)
    }

    @Test func highestGroup() {
        #expect(NameRuleValidator.highestGroup(in: "") == nil)
        #expect(NameRuleValidator.highestGroup(in: "S$1E$12") == 12)
        #expect(NameRuleValidator.highestGroup(in: #"\$3 and $2"#) == 2)
    }

    @Test(arguments: ["(a+)+$", "(a|a)*b", #"(\w+\s?)+$"#, "(a|aa)+$"])
    func catastrophicPatternsAreRejectedQuickly(_ pattern: String) {
        let clock = ContinuousClock()
        var issue: NameRuleIssue?
        let elapsed = clock.measure { issue = NameRuleValidator.issue(rule(pattern)) }
        #expect(issue == .tooSlow)
        // The budget is 60 ms; matching stops soon after it is spent.
        #expect(elapsed < .seconds(1))
    }

    @Test func ordinaryPatternsPassTheTimedRun() {
        #expect(NameRuleValidator.issue(rule(#"[xh]\.?26[45]|HEVC"#)) == nil)
        #expect(NameRuleValidator.issue(rule(#"\[([^\]]{1,40})\]"#, "$1")) == nil)
    }
}
