import SwiftUI

/// A notebook palette: warm paper, dark ink, a little green for actions.
enum CoachTheme {
    static let accent = adaptive(light: 0x345B49, dark: 0xB2CAB7)
    static let caution = adaptive(light: 0x9B6136, dark: 0xE0AD72)
    static let alert = adaptive(light: 0xA84536, dark: 0xE2A39B)
    static let canvas = adaptive(light: 0xF4F0E7, dark: 0x24251F)
    static let surface = adaptive(light: 0xFCFAF4, dark: 0x292B24)
    static let inset = adaptive(light: 0xEAE6DC, dark: 0x33362D)
    static let ink = adaptive(light: 0x2B3029, dark: 0xECE9DF)
    static let muted = adaptive(light: 0x6C7064, dark: 0xB7B7A7)
    static let line = adaptive(light: 0xD3D1C3, dark: 0x494C40)

    static func editorial(_ size: CGFloat) -> Font { .custom("Georgia", size: size) }

    private static func adaptive(light: UInt32, dark: UInt32) -> Color {
        Color(nsColor: NSColor(name: nil) { appearance in
            let value = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? dark : light
            return NSColor(srgbRed: Double((value >> 16) & 255) / 255,
                           green: Double((value >> 8) & 255) / 255,
                           blue: Double(value & 255) / 255, alpha: 1)
        })
    }
}

struct BrandMark: View {
    var size: CGFloat = 38
    var body: some View {
        JanusMark()
            .fill(CoachTheme.accent)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

struct BrandLockup: View {
    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            BrandMark(size: 44)
            Text("Janus")
                .font(.custom("Georgia-Bold", size: 30)).tracking(-0.8)
                .foregroundStyle(CoachTheme.ink)
        }
        .accessibilityElement(children: .combine)
    }
}

struct CoachRule: View {
    var body: some View { Rectangle().fill(CoachTheme.line).frame(height: 1).accessibilityHidden(true) }
}

struct CoachPrimaryButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 12, weight: .medium))
            .padding(.horizontal, 14).padding(.vertical, 9)
            .foregroundStyle(CoachTheme.surface)
            .background(CoachTheme.accent, in: RoundedRectangle(cornerRadius: 5))
            .opacity(!isEnabled ? 0.4 : configuration.isPressed ? 0.78 : 1)
    }
}

/// Text actions have a quiet hover cue without moving the surrounding layout.
struct CoachTextButtonStyle: ButtonStyle {
    @Environment(\.isEnabled) private var isEnabled
    @State private var hovered = false
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .foregroundStyle(CoachTheme.accent)
            .opacity(!isEnabled ? 0.4 : configuration.isPressed ? 0.55 : 1)
            .underline(hovered)
            .onHover { hovered = $0 }
    }
}
