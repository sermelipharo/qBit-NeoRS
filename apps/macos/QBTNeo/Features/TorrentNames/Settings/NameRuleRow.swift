import SwiftUI

/// One row of the rules list: on/off, the category as its tag style, the
/// rule's name and its pattern; a warning symbol when the rule can't be used.
struct NameRuleRow: View {
    @Binding var rule: NameRule
    let issue: NameRuleIssue?

    var body: some View {
        HStack(spacing: Design.Spacing.regular) {
            Toggle(rule.displayName, isOn: $rule.isEnabled)
                .labelsHidden()
                .toggleStyle(.checkbox)
            NameChipLabel(text: rule.category.title, category: rule.category)
                .frame(width: 150, alignment: .leading)
            Text(rule.displayName)
                .lineLimit(1)
                .frame(width: 210, alignment: .leading)
            Text(verbatim: rule.pattern)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let issue {
                Image(systemName: Symbol.exclamationmarkTriangleFill)
                    .foregroundStyle(.yellow)
                    .help(issue.message)
                    .accessibilityLabel(issue.message)
            }
        }
        .opacity(rule.isEnabled ? 1 : 0.55)
        .accessibilityElement(children: .combine)
    }
}
