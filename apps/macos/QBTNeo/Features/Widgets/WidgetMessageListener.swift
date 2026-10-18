import Foundation
import notify

/// Receives the widget extension's Darwin notifications on the main queue.
@MainActor
final class WidgetMessageListener {
    private var tokens: [Int32] = []

    init(handler: @escaping @MainActor (WidgetMessage) -> Void) {
        for message in WidgetMessage.allCases {
            var token: Int32 = 0
            let status = notify_register_dispatch(message.rawValue, &token, .main) { _ in
                MainActor.assumeIsolated { handler(message) }
            }
            if status == NOTIFY_STATUS_OK { tokens.append(token) }
        }
    }

    func cancel() {
        for token in tokens { notify_cancel(token) }
        tokens.removeAll()
    }
}
