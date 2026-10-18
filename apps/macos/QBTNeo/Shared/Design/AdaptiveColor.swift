import AppKit
import SwiftUI

extension Color {
    /// A color that resolves to `light` in light appearances and `dark` in dark ones.
    init(light: UInt32, dark: UInt32) {
        self.init(nsColor: NSColor(light: light, dark: dark))
    }
}

extension NSColor {
    /// A color that resolves to `light` in light appearances and `dark` in dark ones.
    convenience init(light: UInt32, dark: UInt32) {
        let provider: @Sendable (NSAppearance) -> NSColor = { appearance in
            let match = appearance.bestMatch(from: [.darkAqua, .aqua, .vibrantDark, .vibrantLight])
            return NSColor(hex: match == .darkAqua || match == .vibrantDark ? dark : light)
        }
        self.init(name: nil, dynamicProvider: provider)
    }

    convenience init(hex: UInt32, alpha: Double = 1) {
        self.init(
            srgbRed: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            alpha: alpha
        )
    }
}
