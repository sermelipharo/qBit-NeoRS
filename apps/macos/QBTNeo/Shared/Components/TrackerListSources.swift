import SwiftUI

/// Fills a tracker list editor from elsewhere: a list URL (µTorrent compatible,
/// like qBittorrent's "Add trackers" download button) or a text file. The
/// trackers are appended to `text`.
struct TrackerListSources: View {
    @Binding var text: String
    @Environment(AppModel.self) private var model
    @Environment(GUIPreferences.self) private var gui
    @State private var isDownloading = false
    @State private var isChoosingFile = false
    @State private var failure: String?

    var body: some View {
        @Bindable var gui = gui
        VStack(alignment: .leading, spacing: Design.Spacing.tight) {
            LabeledContent("µTorrent compatible list URL:") {
                HStack {
                    TextField("List URL", text: $gui.trackersListURL, prompt: Text("https://"))
                        .labelsHidden()
                        .onSubmit(download)
                    Button("Download", action: download)
                        .disabled(isDownloading || gui.trackersListURL.trimmingCharacters(in: .whitespaces).isEmpty)
                        .overlay {
                            if isDownloading { ProgressView().controlSize(.small) }
                        }
                    Button("Choose File…") { isChoosingFile = true }
                }
            }
            if let failure {
                Label(failure, systemImage: Symbol.exclamationmarkTriangle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .fileImporter(isPresented: $isChoosingFile, allowedContentTypes: [.plainText, .text]) { result in
            failure = nil
            do {
                let list = try AppModel.readTrackerListFile(try result.get())
                text = AppModel.appendingTrackerList(list, to: text)
            } catch {
                failure = String(localized: "Could not read the file: \(error.userMessage)")
            }
        }
    }

    private func download() {
        let url = gui.trackersListURL
        guard !isDownloading, !url.trimmingCharacters(in: .whitespaces).isEmpty else { return }
        isDownloading = true
        failure = nil
        Task {
            defer { isDownloading = false }
            do {
                let list = try await model.downloadTrackerList(url)
                text = AppModel.appendingTrackerList(list, to: text)
            } catch {
                failure = String(localized: "Could not download the trackers list: \(error.userMessage)")
            }
        }
    }
}
