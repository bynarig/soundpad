// Renders the app icon (1024x1024 PNG). Usage: swift make_icon.swift out.png
import AppKit

let size: CGFloat = 1024
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon.png"

let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

// macOS icon grid: 824pt rounded square centred in 1024.
let inset: CGFloat = 100
let rect = NSRect(x: inset, y: inset, width: size - 2 * inset, height: size - 2 * inset)
let path = NSBezierPath(roundedRect: rect, xRadius: 185, yRadius: 185)
NSGradient(colors: [
    NSColor(calibratedRed: 0.17, green: 0.42, blue: 1.0, alpha: 1),
    NSColor(calibratedRed: 0.45, green: 0.22, blue: 0.95, alpha: 1),
])!.draw(in: path, angle: -60)

// 3x3 pad grid.
let pad: CGFloat = 150, gap: CGFloat = 40
let gridW = pad * 3 + gap * 2
let ox = (size - gridW) / 2, oy = (size - gridW) / 2
for r in 0..<3 {
    for c in 0..<3 {
        let lit = (r == 1 && c == 1) || (r == 0 && c == 2)
        let p = NSBezierPath(
            roundedRect: NSRect(x: ox + CGFloat(c) * (pad + gap), y: oy + CGFloat(r) * (pad + gap), width: pad, height: pad),
            xRadius: 34, yRadius: 34)
        NSColor(white: 1, alpha: lit ? 0.95 : 0.28).setFill()
        p.fill()
    }
}
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
