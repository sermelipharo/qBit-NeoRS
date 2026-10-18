import SwiftUI

/// The container background: the system widget background with a faint
/// download-green glow in the top corner. In the vibrant and accented
/// renderings the system replaces it with its own material.
struct WidgetBackground: View {
    var body: some View {
        ZStack {
            Rectangle().fill(.background)
            RadialGradient(colors: [WidgetPalette.download.opacity(0.14), .clear], center: .topLeading, startRadius: 0, endRadius: 220)
        }
    }
}
