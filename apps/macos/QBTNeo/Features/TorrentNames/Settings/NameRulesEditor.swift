import SwiftUI

/// The torrent name rules editor (a sheet of Settings › Interface › Torrent
/// Names): the ordered rules with on/off, drag to reorder, add and remove,
/// reset, import and export; the selected rule's fields; and a live preview
/// of any name with the draft rules. Done saves after validation.
struct NameRulesEditor: View {
    @State private var model: NameRulesEditorModel
    @State private var isImporting = false
    @State private var isExporting = false
    @State private var exportFile: NameRulesFile?
    @Environment(\.dismiss) private var dismiss

    init(names: TorrentNamesModel) {
        _model = State(initialValue: NameRulesEditorModel(names: names))
    }

    var body: some View {
        @Bindable var model = model
        DialogContainer(title: "names.editor.title", confirmTitle: "Done", width: 820, onConfirm: save) {
            Text(String(localized: "names.editor.intro",
                        defaultValue: "Rules run from top to bottom; text a rule matches is not matched again. Drag rules to change the order.",
                        comment: "Name rules editor"))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            VStack(spacing: 0) {
                List(selection: $model.selection) {
                    ForEach($model.rules) { $rule in
                        NameRuleRow(rule: $rule, issue: model.issues[rule.id])
                            .tag(rule.id)
                    }
                    .onMove { model.move(from: $0, to: $1) }
                }
                .listStyle(.bordered(alternatesRowBackgrounds: true))
                .frame(height: 230)
                listBar
            }

            if let rule = model.selectedRule {
                NameRuleDetailForm(model: model, rule: rule)
                    .id(rule.id)
            }

            Divider()

            previewSection

            if let notice = model.notice {
                Label(notice, systemImage: Symbol.infoCircle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .fileImporter(isPresented: $isImporting, allowedContentTypes: [.json]) { result in
            if case .success(let url) = result { model.importRules(from: url) }
        }
        .fileExporter(isPresented: $isExporting, document: exportFile, contentType: .json,
                      defaultFilename: String(localized: "names.export.filename", defaultValue: "Torrent Name Rules",
                                              comment: "Default file name of exported name rules")) { _ in
            exportFile = nil
        }
    }

    /// +/− and the actions menu under the list, like other Mac list editors.
    private var listBar: some View {
        HStack(spacing: 0) {
            Button(String(localized: "names.editor.add", defaultValue: "Add Rule", comment: "Name rules editor"), systemImage: Symbol.plus) {
                model.add()
            }
            Divider().frame(height: 16)
            Button(String(localized: "names.editor.remove", defaultValue: "Remove Rule", comment: "Name rules editor"), systemImage: Symbol.minus) {
                model.removeSelected()
            }
            .disabled(model.selection == nil)
            Spacer()
            Text(String(localized: "names.editor.count", defaultValue: "\(model.enabledCount) of \(model.rules.count) on",
                        comment: "Name rules editor: enabled rules"))
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.trailing, Design.Spacing.regular)
            Menu {
                Button(String(localized: "names.editor.import", defaultValue: "Import Rules…", comment: "Name rules editor")) { isImporting = true }
                Button(String(localized: "names.editor.export", defaultValue: "Export Rules…", comment: "Name rules editor")) { export() }
                Divider()
                Button(String(localized: "names.editor.reset", defaultValue: "Reset to Built-in Rules", comment: "Name rules editor")) {
                    model.resetToDefaults()
                }
                .disabled(model.isDefault)
            } label: {
                Image(systemName: Symbol.ellipsisCircle)
                    .accessibilityLabel(String(localized: "names.editor.more", defaultValue: "More", comment: "Name rules editor menu"))
            }
            .menuIndicator(.hidden)
            .fixedSize()
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .padding(.horizontal, Design.Spacing.tight)
        .frame(height: 24)
        .background(.quinary)
    }

    private var previewSection: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: Design.Spacing.regular) {
            LabeledContent(String(localized: "names.editor.preview", defaultValue: "Try a name:", comment: "Name rules editor preview field")) {
                TextField(String(localized: "names.editor.preview", defaultValue: "Try a name:", comment: "Name rules editor preview field"),
                          text: $model.previewText)
                    .labelsHidden()
                    .font(.system(.body, design: .monospaced))
            }
            ParsedNamePreview(name: model.previewText, parsed: model.preview, showsName: false)
        }
    }

    private func export() {
        guard let data = try? model.exportData() else { return }
        exportFile = NameRulesFile(data: data)
        isExporting = true
    }

    private func save() {
        if model.save() { dismiss() }
    }
}
