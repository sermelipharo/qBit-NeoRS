import AppKit
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

/// A picture of a window taken just before its views are released, shown
/// over the window while they are rebuilt so its return doesn't flash
/// (`WindowContentReleaser`, docs/hidden-ui-memory.md).
///
/// The window server's own image of the window is used (ScreenCaptureKit,
/// own windows need no permission): `cacheDisplay` misses the glass and
/// vibrancy materials. A window-sized bitmap is ~12 MB at 2x, most of what
/// releasing saves, so the picture is kept as PNG data (lossless, a few
/// hundred KB) and decoded only when the window returns. A point-resolution
/// `standby` copy (a quarter of the pixels) stays in the occluded window,
/// which can be uncovered 60–80 ms before the app hears of it.
struct WindowSnapshot: Sendable {
    let png: Data
    let standby: CGImage?
    /// Window frame size in points, backing scale and appearance at capture
    /// time: if any changed, the picture would be stretched or wrong.
    let frameSize: CGSize
    let scale: CGFloat
    let appearance: NSAppearance.Name?

    @MainActor
    func fits(_ window: NSWindow) -> Bool {
        window.frame.size == frameSize && window.backingScaleFactor == scale
            && window.effectiveAppearance.name == appearance && !window.styleMask.contains(.fullScreen)
    }

    /// Decodes the full-resolution picture (tens of ms).
    func decoded() -> CGImage? {
        guard let source = CGImageSourceCreateWithData(png as CFData, nil) else { return nil }
        return CGImageSourceCreateImageAtIndex(source, 0, [kCGImageSourceShouldCacheImmediately: true] as CFDictionary)
    }

    /// Captures `window`; `standbyScale` (fraction of the pixel size) also
    /// keeps a reduced decoded copy. Nil if the window server has no image.
    @MainActor
    static func capture(_ window: NSWindow, standbyScale: CGFloat?) async -> Self? {
        let windowID = CGWindowID(window.windowNumber)
        let frameSize = window.frame.size
        let scale = window.backingScaleFactor
        let appearance = window.effectiveAppearance.name
        guard windowID > 0, let image = await image(of: windowID, size: frameSize, scale: scale) else { return nil }
        return await encode(image, standbyScale: standbyScale, frameSize: frameSize, scale: scale, appearance: appearance)
    }

    private static func image(of windowID: CGWindowID, size: CGSize, scale: CGFloat) async -> CGImage? {
        do {
            let content = try await SCShareableContent.currentProcess
            guard let target = content.windows.first(where: { $0.windowID == windowID }) else { return nil }
            let filter = SCContentFilter(desktopIndependentWindow: target)
            let config = SCStreamConfiguration()
            config.width = Int((size.width * scale).rounded())
            config.height = Int((size.height * scale).rounded())
            config.showsCursor = false
            config.ignoreShadowsSingleWindow = true
            config.capturesShadowsOnly = false
            let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
            return image.width == config.width && image.height == config.height ? image : nil
        } catch {
            return nil
        }
    }

    @concurrent
    private static func encode(_ image: CGImage, standbyScale: CGFloat?, frameSize: CGSize, scale: CGFloat,
                               appearance: NSAppearance.Name) async -> Self? {
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        let standby = standbyScale.flatMap { downscaled(image, by: $0) }
        return Self(png: data as Data, standby: standby, frameSize: frameSize, scale: scale, appearance: appearance)
    }

    private static func downscaled(_ image: CGImage, by factor: CGFloat) -> CGImage? {
        let width = max(1, Int(CGFloat(image.width) * factor))
        let height = max(1, Int(CGFloat(image.height) * factor))
        guard let space = image.colorSpace ?? CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                                      bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        else { return nil }
        context.interpolationQuality = .high
        context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
        return context.makeImage()
    }
}
