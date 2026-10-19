import SwiftUI

/// The container background: the system widget background with soft glows of
/// several colors (blue, violet, pink and teal). In the vibrant and accented
/// renderings the system replaces it with its own material.
struct WidgetBackground: View {
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let strength = colorScheme == .dark ? 1.1 : 1
        ZStack {
            Rectangle().fill(.background)
            glow(.blue, 0.16 * strength, at: .topLeading, radius: 230)
            glow(.purple, 0.12 * strength, at: .bottomTrailing, radius: 240)
            glow(.pink, 0.09 * strength, at: .topTrailing, radius: 170)
            glow(.teal, 0.08 * strength, at: .bottomLeading, radius: 170)
        }
    }

    private func glow(_ color: Color, _ opacity: Double, at center: UnitPoint, radius: CGFloat) -> some View {
        RadialGradient(colors: [color.opacity(opacity), .clear], center: center, startRadius: 0, endRadius: radius)
    }
}
