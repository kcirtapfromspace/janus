import AppKit
import InterviewCoachKit

@MainActor
enum AboutJanus {
    static let repository = URL(string: "https://github.com/kcirtapfromspace/janus")!

    static func show() {
        let version = AppVersion.current
        let credits = NSAttributedString(string: "github.com/kcirtapfromspace/janus", attributes: [
            .link: repository,
            .font: NSFont.systemFont(ofSize: NSFont.smallSystemFontSize),
            .foregroundColor: NSColor.linkColor,
        ])
        NSApp.activate(ignoringOtherApps: true)
        NSApp.orderFrontStandardAboutPanel(options: [
            .applicationName: "Janus",
            .applicationVersion: version.release,
            .version: "build \(version.build)",
            .credits: credits,
        ])
    }
}
