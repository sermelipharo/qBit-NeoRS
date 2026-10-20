import SwiftUI

/// Small borderless button after a value: copy it, or show it in Finder.
struct PropertyValueActionButton: View {
    let action: PropertyValueAction
    let value: String
    @Environment(AppModel.self) private var model

    var body: some View {
        Button(action: perform) {
            switch action {
            case .copy: Label("Copy", systemImage: Symbol.docOnDoc)
            case .reveal: Label("Show in Finder", systemImage: Symbol.folder)
            }
        }
        .labelStyle(.iconOnly)
        .buttonStyle(.borderless)
        .controlSize(.small)
        .foregroundStyle(.secondary)
        .help(help)
    }

    private var help: Text {
        switch action {
        case .copy: Text("Copy")
        case .reveal: Text("Show in Finder")
        }
    }

    private func perform() {
        switch action {
        case .copy: model.copyToPasteboard(value)
        case .reveal(let id): model.openDestination([id])
        }
    }
}
