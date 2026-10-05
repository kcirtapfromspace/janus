#!/usr/bin/env swift
import AppKit
import CoreText
import Foundation

// One vector drawing generates the SwiftUI mark, SVGs, report stamp, and Mac icon.
let project = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let output = URL(fileURLWithPath: CommandLine.arguments.dropFirst().first ?? project.appendingPathComponent("mac/Resources/Brand").path, isDirectory: true)
try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
// Editable Bezier redraw of the approved Roman Rhythm B concept. Each upright
// profile belongs to the quote's perimeter; the closing quote is drawn separately
// so its face stays upright rather than rotating with the punctuation.
// Reference: mac/Resources/Brand/Concepts/Janus/roman-rhythm-taller.png.
func referencePoint(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
    let scale: CGFloat = 84 / 390
    return CGPoint(x: 9.6 + (x - 300) * scale, y: 8 + (y - 125) * scale)
}
let mark = CGMutablePath()
// Opening quote: the sweep becomes the forehead, nose, lips, and chin.
mark.move(to: referencePoint(466, 125))
mark.addCurve(to: referencePoint(324, 207), control1: referencePoint(407, 120), control2: referencePoint(336, 161))
mark.addCurve(to: referencePoint(324, 229), control1: referencePoint(320, 219), control2: referencePoint(331, 218))
mark.addLine(to: referencePoint(305, 263))
mark.addCurve(to: referencePoint(301, 278), control1: referencePoint(301, 269), control2: referencePoint(298, 274))
mark.addCurve(to: referencePoint(319, 289), control1: referencePoint(304, 284), control2: referencePoint(319, 281))
mark.addCurve(to: referencePoint(320, 305), control1: referencePoint(320, 294), control2: referencePoint(316, 301))
mark.addCurve(to: referencePoint(321, 314), control1: referencePoint(316, 309), control2: referencePoint(317, 312))
mark.addLine(to: referencePoint(326, 317))
mark.addCurve(to: referencePoint(327, 329), control1: referencePoint(322, 322), control2: referencePoint(322, 326))
mark.addCurve(to: referencePoint(333, 350), control1: referencePoint(334, 335), control2: referencePoint(337, 342))
mark.addCurve(to: referencePoint(356, 374), control1: referencePoint(329, 365), control2: referencePoint(335, 374))
mark.addLine(to: referencePoint(466, 374))
mark.addLine(to: referencePoint(466, 261))
mark.addLine(to: referencePoint(412, 261))
mark.addCurve(to: referencePoint(466, 198), control1: referencePoint(406, 241), control2: referencePoint(429, 211))
mark.closeSubpath()
// Closing quote: the square terminal leads into an outward-facing profile.
mark.move(to: referencePoint(504, 261))
mark.addLine(to: referencePoint(614, 261))
mark.addCurve(to: referencePoint(634, 280), control1: referencePoint(627, 261), control2: referencePoint(632, 268))
mark.addLine(to: referencePoint(648, 306))
mark.addCurve(to: referencePoint(651, 328), control1: referencePoint(654, 317), control2: referencePoint(646, 320))
mark.addLine(to: referencePoint(672, 359))
mark.addCurve(to: referencePoint(659, 374), control1: referencePoint(678, 368), control2: referencePoint(671, 372))
mark.addCurve(to: referencePoint(654, 388), control1: referencePoint(653, 375), control2: referencePoint(652, 381))
mark.addCurve(to: referencePoint(646, 400), control1: referencePoint(656, 394), control2: referencePoint(651, 398))
mark.addLine(to: referencePoint(650, 405))
mark.addCurve(to: referencePoint(642, 420), control1: referencePoint(651, 411), control2: referencePoint(646, 416))
mark.addCurve(to: referencePoint(638, 442), control1: referencePoint(636, 425), control2: referencePoint(642, 433))
mark.addCurve(to: referencePoint(614, 460), control1: referencePoint(636, 455), control2: referencePoint(625, 460))
mark.addCurve(to: referencePoint(501, 515), control1: referencePoint(588, 491), control2: referencePoint(551, 514))
mark.addLine(to: referencePoint(501, 438))
mark.addCurve(to: referencePoint(557, 376), control1: referencePoint(531, 428), control2: referencePoint(554, 405))
mark.addLine(to: referencePoint(504, 376))
mark.closeSubpath()

func number(_ value: CGFloat) -> String { String(format: "%.3f", Double(value)) }
func svgPath(_ path: CGPath) -> String {
    var commands: [String] = []
    func point(_ p: CGPoint) -> String { "\(number(p.x)) \(number(p.y))" }
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
func wordmark(_ size: CGFloat) -> CGPath {
    let font = CTFontCreateWithName("Georgia-Bold" as CFString, size, nil)
    let text = NSAttributedString(string: "Janus", attributes: [NSAttributedString.Key(kCTFontAttributeName as String): font])
    let path = CGMutablePath()
    for run in CTLineGetGlyphRuns(CTLineCreateWithAttributedString(text)) as! [CTRun] {
        let count = CTRunGetGlyphCount(run)
        var glyphs = [CGGlyph](repeating: 0, count: count)
        var positions = [CGPoint](repeating: .zero, count: count)
        CTRunGetGlyphs(run, CFRange(location: 0, length: count), &glyphs)
        CTRunGetPositions(run, CFRange(location: 0, length: count), &positions)
        for index in 0..<count {
            if let glyph = CTFontCreatePathForGlyph(font, glyphs[index], nil) {
                path.addPath(glyph, transform: CGAffineTransform(translationX: positions[index].x, y: positions[index].y))
            }
        }
    }
    return path
}
func write(_ text: String, _ filename: String) throws {
    try text.write(to: output.appendingPathComponent(filename), atomically: true, encoding: .utf8)
}
let geometry = svgPath(mark)
for (filename, color) in [("mark.svg", "#345B49"), ("mark-dark.svg", "#B2CAB7")] {
    try write("""
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 100 100" role="img" aria-labelledby="title">
      <title id="title">Janus: Roman profiles in opposing quotation marks</title><path fill="\(color)" d="\(geometry)"/>
    </svg>
    """, filename)
}
let lettering = wordmark(54)
for (filename, markColor, typeColor) in [("wordmark.svg", "#345B49", "#2B3029"), ("wordmark-dark.svg", "#B2CAB7", "#ECE9DF")] {
    try write("""
    <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 \(Int(ceil(lettering.boundingBoxOfPath.maxX + 108))) 100" role="img" aria-labelledby="title">
      <title id="title">Janus</title>
      <path transform="translate(4 12) scale(.7)" fill="\(markColor)" d="\(geometry)"/>
      <path transform="translate(92 70) scale(1 -1)" fill="\(typeColor)" d="\(svgPath(lettering))"/>
    </svg>
    """, filename)
}
try write("""
<div class="janus-brand" id="janus-brand"><svg viewBox="0 0 100 100" aria-hidden="true"><path d="\(geometry)"/></svg><span>Janus</span></div>
""", "report-brand.html")

// Generate native geometry instead of maintaining a second drawing by hand.
var native: [String] = []
func point(_ p: CGPoint) -> String { "CGPoint(x: \(number(p.x)), y: \(number(p.y)))" }
mark.applyWithBlock { element in
    let e = element.pointee
    switch e.type {
    case .moveToPoint: native.append("path.move(to: \(point(e.points[0])))")
    case .addLineToPoint: native.append("path.addLine(to: \(point(e.points[0])))")
    case .addQuadCurveToPoint: native.append("path.addQuadCurve(to: \(point(e.points[1])), control: \(point(e.points[0])))")
    case .addCurveToPoint: native.append("path.addCurve(to: \(point(e.points[2])), control1: \(point(e.points[0])), control2: \(point(e.points[1])))")
    case .closeSubpath: native.append("path.closeSubpath()")
    @unknown default: fatalError("Unknown path element")
    }
}
let source = """
// Generated by scripts/make-brand-assets.swift. Edit the generator, then regenerate.
import SwiftUI

struct JanusMark: Shape {
    private static let geometry: CGPath = {
        let path = CGMutablePath()
        \(native.joined(separator: "\n        "))
        return path
    }()

    func path(in rect: CGRect) -> Path {
        var transform = CGAffineTransform(a: rect.width / 100, b: 0, c: 0, d: rect.height / 100,
                                         tx: rect.minX, ty: rect.minY)
        return Path(Self.geometry.copy(using: &transform)!)
    }

    @MainActor static let menuBarImage: NSImage = {
        let image = NSImage(size: NSSize(width: 20, height: 20), flipped: true) { rect in
            let context = NSGraphicsContext.current!.cgContext
            context.saveGState()
            context.scaleBy(x: rect.width / 100, y: rect.height / 100)
            context.setFillColor(NSColor.black.cgColor)
            context.addPath(geometry)
            context.fillPath()
            context.restoreGState()
            return true
        }
        image.isTemplate = true
        return image
    }()
}

"""
try source.write(to: project.appendingPathComponent("mac/Sources/InterviewCoach/JanusMark.swift"), atomically: true, encoding: .utf8)

func color(_ rgb: UInt32) -> NSColor {
    NSColor(srgbRed: Double((rgb >> 16) & 255) / 255, green: Double((rgb >> 8) & 255) / 255,
            blue: Double(rgb & 255) / 255, alpha: 1)
}
func png(width: Int, height: Int, drawing: (CGContext) -> Void) -> Data {
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
                              bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                              colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let context = NSGraphicsContext.current!.cgContext
    context.translateBy(x: 0, y: CGFloat(height))
    context.scaleBy(x: 1, y: -1)
    drawing(context)
    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}
func draw(_ path: CGPath, in context: CGContext, x: CGFloat, y: CGFloat, scale: CGFloat, rgb: UInt32, flip: Bool = false) {
    context.saveGState()
    context.translateBy(x: x, y: y)
    context.scaleBy(x: scale, y: flip ? -scale : scale)
    context.setFillColor(color(rgb).cgColor)
    context.addPath(path)
    context.fillPath()
    context.restoreGState()
}
func icon(_ pixels: Int) -> Data {
    png(width: pixels, height: pixels) { context in
        context.scaleBy(x: CGFloat(pixels) / 1024, y: CGFloat(pixels) / 1024)
        context.setFillColor(color(0xF4F0E7).cgColor)
        context.addPath(CGPath(roundedRect: CGRect(x: 64, y: 64, width: 896, height: 896), cornerWidth: 216, cornerHeight: 216, transform: nil))
        context.fillPath()
        draw(mark, in: context, x: 64, y: 64, scale: 8.96, rgb: 0x345B49)
    }
}
let iconset = output.appendingPathComponent("AppIcon.iconset", isDirectory: true)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for points in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 2 ? "@2x" : ""
        try icon(points * scale).write(to: iconset.appendingPathComponent("icon_\(points)x\(points)\(suffix).png"))
    }
}
try icon(1024).write(to: output.appendingPathComponent("app-icon.png"))
let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
process.arguments = ["-c", "icns", iconset.path, "-o", output.appendingPathComponent("AppIcon.icns").path]
try process.run()
process.waitUntilExit()
precondition(process.terminationStatus == 0)
try FileManager.default.removeItem(at: iconset)
let board = png(width: 1800, height: 1120) { context in
    context.setFillColor(color(0xF4F0E7).cgColor)
    context.fill(CGRect(x: 0, y: 0, width: 1800, height: 1120))
    draw(mark, in: context, x: 370, y: 190, scale: 3, rgb: 0x345B49)
    draw(wordmark(170), in: context, x: 740, y: 400, scale: 1, rgb: 0x2B3029, flip: true)
    context.setFillColor(color(0x24251F).cgColor)
    context.fill(CGRect(x: 0, y: 700, width: 1800, height: 420))
    draw(mark, in: context, x: 150, y: 810, scale: 1.9, rgb: 0xB2CAB7)
    draw(wordmark(112), in: context, x: 400, y: 945, scale: 1, rgb: 0xECE9DF, flip: true)
    context.setFillColor(color(0xF4F0E7).cgColor)
    context.addPath(CGPath(roundedRect: CGRect(x: 1190, y: 800, width: 220, height: 220), cornerWidth: 48, cornerHeight: 48, transform: nil))
    context.fillPath()
    draw(mark, in: context, x: 1190, y: 800, scale: 2.2, rgb: 0x345B49)
    draw(mark, in: context, x: 1500, y: 860, scale: 1, rgb: 0xB2CAB7)
}
try board.write(to: output.appendingPathComponent("identity.png"))
