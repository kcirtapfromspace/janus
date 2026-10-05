#!/usr/bin/env swift
import AppKit
import CoreText
import Foundation

// Editable vector interpretation of the Roundnote identity concept. This does not replace
// the application's current identity. Run from any directory to regenerate these assets.
let output = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
let symbol = CGMutablePath()
symbol.move(to: CGPoint(x: 8, y: 8))
symbol.addLine(to: CGPoint(x: 56, y: 8))
symbol.addCurve(to: CGPoint(x: 90, y: 39), control1: CGPoint(x: 79, y: 8), control2: CGPoint(x: 90, y: 21))
symbol.addCurve(to: CGPoint(x: 64, y: 68), control1: CGPoint(x: 90, y: 55), control2: CGPoint(x: 80, y: 65))
for point in [(90.0, 90.0), (96, 90), (96, 93), (65, 93), (42, 66), (34, 66), (34, 78)] {
    symbol.addLine(to: CGPoint(x: point.0, y: point.1))
}
symbol.addCurve(to: CGPoint(x: 44, y: 90), control1: CGPoint(x: 34, y: 87), control2: CGPoint(x: 38, y: 90))
for point in [(44.0, 93.0), (8, 93), (8, 90)] { symbol.addLine(to: CGPoint(x: point.0, y: point.1)) }
symbol.addCurve(to: CGPoint(x: 18, y: 78), control1: CGPoint(x: 15, y: 90), control2: CGPoint(x: 18, y: 86))
symbol.addLine(to: CGPoint(x: 18, y: 23))
symbol.addCurve(to: CGPoint(x: 8, y: 11), control1: CGPoint(x: 18, y: 16), control2: CGPoint(x: 14, y: 11))
symbol.closeSubpath()
// The counter is an actual knockout: it remains transparent in one-color use.
symbol.move(to: CGPoint(x: 50, y: 23))
symbol.addCurve(to: CGPoint(x: 30, y: 39), control1: CGPoint(x: 36, y: 23), control2: CGPoint(x: 30, y: 29))
symbol.addCurve(to: CGPoint(x: 37, y: 51), control1: CGPoint(x: 30, y: 45), control2: CGPoint(x: 33, y: 49))
symbol.addLine(to: CGPoint(x: 31, y: 62))
symbol.addLine(to: CGPoint(x: 48, y: 53))
symbol.addLine(to: CGPoint(x: 52, y: 53))
symbol.addCurve(to: CGPoint(x: 73, y: 39), control1: CGPoint(x: 66, y: 53), control2: CGPoint(x: 73, y: 48))
symbol.addCurve(to: CGPoint(x: 50, y: 23), control1: CGPoint(x: 73, y: 29), control2: CGPoint(x: 67, y: 23))
symbol.closeSubpath()

func svgPath(_ path: CGPath) -> String {
    var commands: [String] = []
    func point(_ p: CGPoint) -> String { String(format: "%.3f %.3f", Double(p.x), Double(p.y)) }
    path.applyWithBlock { element in
        let e = element.pointee
        switch e.type {
        case .moveToPoint: commands.append("M\(point(e.points[0]))")
        case .addLineToPoint: commands.append("L\(point(e.points[0]))")
        case .addQuadCurveToPoint: commands.append("Q\(point(e.points[0])) \(point(e.points[1]))")
        case .addCurveToPoint: commands.append("C\(point(e.points[0])) \(point(e.points[1])) \(point(e.points[2]))")
        case .closeSubpath: commands.append("Z")
        @unknown default: fatalError("Unknown path element")
        }
    }
    return commands.joined(separator: " ")
}

// Outline the wordmark so the SVG renders consistently without requiring an installed font.
let font = CTFontCreateWithName("Georgia-Bold" as CFString, 68, nil)
let letters = Array("roundnote".utf16)
var glyphs = [CGGlyph](repeating: 0, count: letters.count)
precondition(CTFontGetGlyphsForCharacters(font, letters, &glyphs, letters.count))
var advances = [CGSize](repeating: .zero, count: glyphs.count)
CTFontGetAdvancesForGlyphs(font, .horizontal, glyphs, &advances, glyphs.count)
let wordmark = CGMutablePath()
var x: CGFloat = 0
for index in glyphs.indices {
    if let path = CTFontCreatePathForGlyph(font, glyphs[index], nil) {
        wordmark.addPath(path, transform: CGAffineTransform(translationX: x, y: 0))
    }
    x += advances[index].width
}
let width = Int(ceil(x + 140))
let symbolData = svgPath(symbol)
let wordmarkData = svgPath(wordmark)
func save(_ text: String, as filename: String) throws {
    try text.write(to: output.appendingPathComponent(filename), atomically: true, encoding: .utf8)
}
try save("""
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" role="img" aria-labelledby="title">
  <title id="title">Roundnote conversation monogram</title>
  <path fill="#345B49" fill-rule="evenodd" d="\(symbolData)"/>
</svg>
""", as: "mark.svg")
for (filename, markColor, typeColor) in [("wordmark.svg", "#345B49", "#2B3029"),
                                        ("wordmark-dark.svg", "#B2CAB7", "#ECE9DF")] {
    try save("""
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 \(width) 112" role="img" aria-labelledby="title">
      <title id="title">Roundnote</title>
      <path transform="translate(0 6)" fill="\(markColor)" fill-rule="evenodd" d="\(symbolData)"/>
      <path transform="translate(126 85) scale(1 -1)" fill="\(typeColor)" d="\(wordmarkData)"/>
    </svg>
    """, as: filename)
}

// Native app icon preview, with the same transparent counter and generous small-size padding.
let pixels = 1024
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
                          bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                          colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let tile = CGRect(x: 64, y: 64, width: 896, height: 896)
NSColor(srgbRed: 244/255, green: 240/255, blue: 231/255, alpha: 1).setFill()
NSBezierPath(roundedRect: tile, xRadius: 204, yRadius: 204).fill()
let context = NSGraphicsContext.current!.cgContext
context.translateBy(x: 170, y: 856)
context.scaleBy(x: 6.8, y: -6.8)
context.setFillColor(NSColor(srgbRed: 52/255, green: 91/255, blue: 73/255, alpha: 1).cgColor)
context.addPath(symbol)
context.drawPath(using: .eoFill)
NSGraphicsContext.restoreGraphicsState()
try rep.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent("app-icon.png"))
