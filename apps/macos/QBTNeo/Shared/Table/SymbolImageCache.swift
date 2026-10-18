import AppKit

/// SF Symbol images shared by every table cell. Template images are tinted
/// per view (`contentTintColor`), so one image per symbol name is enough; the
/// set of names is small and fixed (states, label icons, chevrons).
@MainActor
enum SymbolImageCache {
    private static var images: [String: NSImage] = [:]

    static func image(_ name: String) -> NSImage? {
        if let cached = images[name] { return cached }
        guard let image = NSImage(systemSymbolName: name, accessibilityDescription: nil) else { return nil }
        image.isTemplate = true
        // Label icons come from the user's choice: keep the cache bounded anyway.
        if images.count > 256 { images.removeAll(keepingCapacity: true) }
        images[name] = image
        return image
    }
}
