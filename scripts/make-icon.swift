// Usage: swift scripts/make-icon.swift assets/AppIcon.appiconset/icon_1024.png

import AppKit
import CoreGraphics

// Renders the app icon at 1024x1024: a continuous-corner squircle with a deep
// blue gradient, a soft inner glow, and a white "q" whose descender ends in a
// download arrow.
let size = 1024.0
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon_1024.png"

let cs = CGColorSpace(name: CGColorSpace.displayP3)!
let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                    space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!

func color(_ r: Double, _ g: Double, _ b: Double, _ a: Double = 1) -> CGColor {
    CGColor(colorSpace: cs, components: [r, g, b, a])!
}

// macOS icon grid: 824pt body inside 1024 canvas with ~100pt margins.
let inset = 100.0
let body = CGRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let radius = body.width * 0.2237
let shape = CGPath(roundedRect: body, cornerWidth: radius, cornerHeight: radius, transform: nil)

// Drop shadow.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: color(0, 0, 0, 0.35))
ctx.addPath(shape)
ctx.setFillColor(color(0.05, 0.2, 0.55))
ctx.fillPath()
ctx.restoreGState()

// Background gradient.
ctx.saveGState()
ctx.addPath(shape)
ctx.clip()
let bg = CGGradient(colorsSpace: cs, colors: [color(0.18, 0.55, 1.0), color(0.09, 0.30, 0.86), color(0.05, 0.14, 0.52)] as CFArray,
                    locations: [0, 0.55, 1])!
ctx.drawLinearGradient(bg, start: CGPoint(x: size / 2, y: body.maxY), end: CGPoint(x: size / 2, y: body.minY), options: [])
// Top sheen.
let sheen = CGGradient(colorsSpace: cs, colors: [color(1, 1, 1, 0.28), color(1, 1, 1, 0)] as CFArray, locations: [0, 1])!
ctx.drawRadialGradient(sheen, startCenter: CGPoint(x: size * 0.38, y: body.maxY - 60), startRadius: 0,
                       endCenter: CGPoint(x: size * 0.38, y: body.maxY - 60), endRadius: 560, options: [])
ctx.restoreGState()

// Glyph: a "q" (ring + stem) whose descender ends in a download arrow,
// drawn in one transparency layer so it casts a single shadow.
let cx = size * 0.462, cy = size * 0.565, r = 176.0, stroke = 96.0
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 22, color: color(0, 0.04, 0.22, 0.5))
ctx.beginTransparencyLayer(auxiliaryInfo: nil)
ctx.setStrokeColor(color(1, 1, 1))
ctx.setFillColor(color(1, 1, 1))
ctx.setLineWidth(stroke)
ctx.setLineCap(.round)
ctx.addEllipse(in: CGRect(x: cx - r, y: cy - r, width: 2 * r, height: 2 * r))
let stemX = cx + r
let headTop = size * 0.335
ctx.move(to: CGPoint(x: stemX, y: cy + r))
ctx.addLine(to: CGPoint(x: stemX, y: headTop + 20))
ctx.strokePath()
let head = 132.0
let tip = size * 0.165
ctx.move(to: CGPoint(x: stemX - head, y: headTop))
ctx.addLine(to: CGPoint(x: stemX + head, y: headTop))
ctx.addLine(to: CGPoint(x: stemX, y: tip))
ctx.closePath()
ctx.fillPath()
ctx.endTransparencyLayer()
ctx.restoreGState()

let img = ctx.makeImage()!
let rep = NSBitmapImageRep(cgImage: img)
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
print(out)
