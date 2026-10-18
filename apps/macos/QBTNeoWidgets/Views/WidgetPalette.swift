import SwiftUI

/// Widget colors. Fixed or system colors only: widget views are archived and
/// drawn by another process, so dynamic AppKit color providers are avoided.
enum WidgetPalette {
    /// The app's download graph color (qBittorrent pen `rgb(134,196,63)`).
    static let download = Color(red: 134 / 255, green: 196 / 255, blue: 63 / 255)
    /// The app's upload graph color (qBittorrent pen `rgb(50,153,255)`).
    static let upload = Color(red: 50 / 255, green: 153 / 255, blue: 1)
    /// Alternative speed limits (the app's tortoise).
    static let altSpeed = Color.orange
}
