import SwiftUI

/// "Session paused" status bar item; clicking it resumes the session.
struct SessionPausedIndicator: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.status.isPaused {
            Button { model.resumeSession() } label: {
                Label("Session paused", systemImage: Symbol.pauseCircleFill)
                    .foregroundStyle(.orange)
            }
            .buttonStyle(.borderless)
            .help("Click to resume the session")
        }
    }
}
