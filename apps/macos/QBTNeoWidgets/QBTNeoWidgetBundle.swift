import SwiftUI
import WidgetKit

/// qBit NeoRS widgets for the desktop and Notification Center. They only
/// read the snapshot the app writes into the shared App Group container;
/// buttons ask the running app through Darwin notifications.
@main
struct QBTNeoWidgetBundle: WidgetBundle {
    var body: some Widget {
        TransfersWidget()
        SpeedWidget()
        TorrentWidget()
    }
}
