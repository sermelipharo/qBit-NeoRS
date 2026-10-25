import SwiftUI

/// Left pane of the Add Torrent dialog, in the order of the decisions:
/// where it goes (and whether its category decides that), its category and
/// tags, whether it starts (and when it stops), then the rarer options under
/// "More options".
struct AddTorrentOptionsForm: View {
    @Bindable var dialog: AddTorrentModel
    @Environment(GUIPreferences.self) private var gui
    @State private var showsTags = false

    var body: some View {
        Form {
            Section {
                LabeledContent("Save to") {
                    FolderMenu(title: "Save path", path: $dialog.savePath, history: dialog.savePathHistory,
                               remembers: $dialog.rememberLastSavePath)
                        .fixedSize()
                        .disabled(dialog.autoTMM)
                }
                Toggle("Use the category's folder", isOn: $dialog.autoTMM)
                    .help("Automatic mode means that various torrent properties (e.g. save path) will be decided by the associated category")
                if !dialog.autoTMM {
                    Toggle("Keep incomplete files elsewhere", isOn: $dialog.useDownloadPath)
                    if dialog.useDownloadPath {
                        LabeledContent("Incomplete files") {
                            FolderMenu(title: "Incomplete path", path: $dialog.downloadPath, history: dialog.downloadPathHistory)
                                .fixedSize()
                        }
                    }
                }
            }
            Section {
                LabeledContent("Category") {
                    Menu(dialog.category.isEmpty ? String(localized: "(None)") : dialog.category) {
                        Picker("Category", selection: $dialog.category) {
                            ForEach(dialog.categoryChoices, id: \.self) { name in
                                Text(name.isEmpty ? String(localized: "(None)") : name).tag(name)
                            }
                        }
                        .pickerStyle(.inline)
                        .labelsHidden()
                        Divider()
                        Toggle("Use as Default Category", isOn: $dialog.setAsDefaultCategory)
                    }
                    .fixedSize()
                }
                LabeledContent("Tags") {
                    HStack(spacing: Design.Spacing.tight) {
                        Text(dialog.tags.isEmpty ? String(localized: "None") : dialog.tags.joined(separator: ", "))
                            .foregroundStyle(dialog.tags.isEmpty ? .secondary : .primary)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        Button("Edit Tags…", systemImage: Symbol.plus) { showsTags = true }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.borderless)
                            .help("Add/remove tags")
                    }
                }
            }
            Section {
                Toggle("Start torrent", isOn: $dialog.startTorrent)
                if dialog.startTorrent {
                    Picker("Stop after", selection: $dialog.stopCondition) {
                        Text("Never").tag(StopCondition.none)
                        Text("Metadata received").tag(StopCondition.metadataReceived)
                            .selectionDisabled(dialog.hasMetadata)
                        Text("Files checked").tag(StopCondition.filesChecked)
                    }
                    .help("""
                        None: no stop condition is set. Metadata received: the torrent will stop after metadata is received. \
                        Files checked: the torrent will stop after files are initially checked.
                        """)
                }
                Toggle("Download in sequential order", isOn: $dialog.sequential)
            }
            Section {
                moreOptionsHeader
                if gui.addTorrentMoreOptions {
                    Toggle("Add to top of queue", isOn: $dialog.addToQueueTop)
                    Toggle("Skip hash check", isOn: $dialog.skipHashCheck)
                    Toggle("First and last pieces first", isOn: $dialog.firstLastPiece)
                        .help("Download first and last pieces first")
                    Picker("Content layout", selection: $dialog.contentLayout) {
                        Text("Original").tag(ContentLayout.original)
                        Text("Create subfolder").tag(ContentLayout.subfolder)
                        Text("Don't create subfolder").tag(ContentLayout.noSubfolder)
                    }
                }
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $showsTags) {
            TagsPickerSheet(tags: $dialog.tags)
        }
    }

    /// The whole row toggles the options, not only the chevron.
    private var moreOptionsHeader: some View {
        Button {
            withAnimation(.snappy(duration: 0.2)) { gui.addTorrentMoreOptions.toggle() }
        } label: {
            HStack(spacing: Design.Spacing.small) {
                Text("More options")
                    .foregroundStyle(.primary)
                let summary = dialog.moreOptionsSummary
                if !gui.addTorrentMoreOptions, !summary.isEmpty {
                    Text(summary)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                Spacer(minLength: Design.Spacing.small)
                Image(systemName: Symbol.chevronRight)
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .rotationEffect(.degrees(gui.addTorrentMoreOptions ? 90 : 0))
            }
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityValue(gui.addTorrentMoreOptions ? Text("Expanded") : Text("Collapsed"))
    }
}
