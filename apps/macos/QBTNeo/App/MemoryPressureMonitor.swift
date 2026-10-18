import Darwin
import Dispatch
import Foundation

/// Watches the system's memory pressure. On a warning it posts
/// `warningNotification` (closed windows drop their views right away, see
/// `ReleasedWhileHidden`) and then hands free malloc pages back to the system.
@MainActor
final class MemoryPressureMonitor {
    static let shared = MemoryPressureMonitor()
    static let warningNotification = Notification.Name("QBTMemoryPressureWarning")

    private var source: (any DispatchSourceMemoryPressure)?

    private init() {}

    func start() {
        guard source == nil else { return }
        let source = DispatchSource.makeMemoryPressureSource(eventMask: [.warning, .critical], queue: .main)
        source.setEventHandler {
            MainActor.assumeIsolated { Self.handlePressure() }
        }
        source.activate()
        self.source = source
    }

    private static func handlePressure() {
        NotificationCenter.default.post(name: warningNotification, object: nil)
        // Views released by the notification are freed on the next update;
        // give that a moment before returning empty pages.
        Task {
            try? await Task.sleep(for: .seconds(1))
            malloc_zone_pressure_relief(nil, 0)
        }
    }
}
