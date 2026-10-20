import SwiftUI

/// Edits the selected rule: name, category (shown in its tag style),
/// pattern with live validation, tag text and case sensitivity. Works on a
/// local copy that is handed to the model on every change.
struct NameRuleDetailForm: View {
    let model: NameRulesEditorModel
    @State private var draft: NameRule

    init(model: NameRulesEditorModel, rule: NameRule) {
        self.model = model
        _draft = State(initialValue: rule)
    }

    var body: some View {
        Grid(alignment: .leadingFirstTextBaseline, horizontalSpacing: Design.Spacing.regular, verticalSpacing: Design.Spacing.regular) {
            GridRow {
                label(String(localized: "names.editor.name", defaultValue: "Name:", comment: "Name rules editor field"))
                TextField(String(localized: "names.editor.name", defaultValue: "Name:", comment: "Name rules editor field"), text: $draft.name,
                          prompt: Text(verbatim: NameRuleDefaults.localizedName(of: draft.id) ?? ""))
                    .labelsHidden()
            }
            GridRow {
                label(String(localized: "names.editor.category", defaultValue: "Category:", comment: "Name rules editor field"))
                HStack(spacing: Design.Spacing.regular) {
                    Picker(String(localized: "names.editor.category", defaultValue: "Category:", comment: "Name rules editor field"),
                           selection: $draft.category) {
                        ForEach(NameRuleCategory.allCases) { category in
                            Text(category.title).tag(category)
                        }
                    }
                    .labelsHidden()
                    .fixedSize()
                    // How this category's tags look.
                    NameChipLabel(text: draft.category.title, category: draft.category)
                        .accessibilityHidden(true)
                }
            }
            GridRow {
                label(String(localized: "names.editor.pattern", defaultValue: "Pattern:", comment: "Name rules editor field"))
                VStack(alignment: .leading, spacing: SettingsLayout.footnoteSpacing) {
                    TextField(String(localized: "names.editor.pattern", defaultValue: "Pattern:", comment: "Name rules editor field"),
                              text: $draft.pattern)
                        .labelsHidden()
                        .font(.system(.body, design: .monospaced))
                        .autocorrectionDisabled()
                    if let issue = model.issues[draft.id] {
                        Label(issue.message, systemImage: Symbol.exclamationmarkTriangleFill)
                            .font(.caption)
                            .foregroundStyle(.red)
                    } else {
                        Text(String(localized: "names.editor.pattern.help",
                                    defaultValue: "A regular expression, matched as a whole token: letters and digits never touch it.",
                                    comment: "Name rules editor help"))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let note = model.webNote(for: draft.id) {
                        Label(note, systemImage: Symbol.exclamationmarkTriangle)
                            .font(.caption)
                            .foregroundStyle(.orange)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            GridRow {
                label(String(localized: "names.editor.template", defaultValue: "Tag text:", comment: "Name rules editor field"))
                VStack(alignment: .leading, spacing: SettingsLayout.footnoteSpacing) {
                    TextField(String(localized: "names.editor.template", defaultValue: "Tag text:", comment: "Name rules editor field"),
                              text: $draft.template,
                              prompt: Text(String(localized: "names.editor.template.prompt", defaultValue: "The matched text",
                                                  comment: "Name rules editor placeholder")))
                        .labelsHidden()
                        .font(.system(.body, design: .monospaced))
                        .disabled(draft.category == .hidden)
                    Text(String(localized: "names.editor.template.help", defaultValue: "$1, $2… insert the pattern's groups; $0 is the whole match.",
                                comment: "Name rules editor help"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            GridRow {
                Color.clear.gridCellUnsizedAxes([.horizontal, .vertical])
                Toggle(String(localized: "names.editor.caseSensitive", defaultValue: "Case sensitive", comment: "Name rules editor option"),
                       isOn: $draft.isCaseSensitive)
            }
        }
        .onChange(of: draft) { model.update(draft) }
    }

    private func label(_ text: String) -> some View {
        Text(text)
            .gridColumnAlignment(.trailing)
    }
}
