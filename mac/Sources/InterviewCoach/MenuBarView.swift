import AppKit
import InterviewCoachKit
import SwiftUI

/// The menu under the menu-bar icon, laid out like a native app's status menu: what's happening
/// now, the main actions with shortcuts, recent interviews, then setup, updates and quit. It's
/// rebuilt from the current state every time it opens.
struct MenuBarMenu: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        status
        Divider()

        if case .recording = model.phase {
            Button { model.stopRecording() } label: { Label("Stop Recording", systemImage: "stop.circle") }
                .keyboardShortcut(".")
        } else {
            Button { show("record") } label: { Label("Record Interview…", systemImage: "record.circle") }
                .keyboardShortcut("r")
                .disabled(model.phase.isBusy)
        }
        Button { show("main") } label: { Label("Open Janus", systemImage: "macwindow") }
            .keyboardShortcut("o")
        Button { model.importRecording() } label: { Label("Import Recording…", systemImage: "square.and.arrow.down") }
            .keyboardShortcut("i")
            .disabled(model.phase.isBusy)

        Divider()
        Menu("Recent Interviews") {
            if model.sessions.isEmpty {
                Text("No interviews yet")
            }
            ForEach(model.sessions.prefix(8)) { session in
                Button(recentTitle(session)) {
                    model.selection = session.id
                    show("main")
                }
            }
        }

        Divider()
        Button(setupTitle) { show("setup") }
            .keyboardShortcut(",")
        AppearanceMenu()
        if model.updater.isAvailable {
            Button { model.updater.checkForUpdates() } label: {
                Label(model.updateReady.map { "Update to \($0) Ready" } ?? "Check for Updates…",
                      systemImage: model.updateReady == nil ? "arrow.triangle.2.circlepath" : "arrow.down.circle")
            }
            .disabled(model.phase.isRecording)
        }
        Button("About Janus") {
            NSApp.activate(ignoringOtherApps: true)
            NSApp.orderFrontStandardAboutPanel(nil)
        }

        Divider()
        Button("Quit Janus") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
            .disabled(model.phase.isRecording)
    }

    /// The first line: a coloured dot and what Janus is doing (not clickable).
    @ViewBuilder private var status: some View {
        let (color, text) = statusLine
        Button {} label: {
            Label { Text(text) } icon: { Image(nsImage: dot(color)) }
        }
        .disabled(true)
    }

    private var statusLine: (NSColor, String) {
        if model.captureProblem != nil {
            return (.systemOrange, "Your mic isn't being recorded")
        }
        switch model.phase {
        case .recording(let since):
            return (.systemRed, "Recording since \(since.formatted(date: .omitted, time: .shortened))")
        case .working(let label):
            return (.systemBlue, label)
        default:
            break
        }
        if let setup = model.setup, !setup.ready {
            return (.systemOrange, setup.remaining == 1 ? "Setup: 1 thing left" : "Setup: \(setup.remaining) things left")
        }
        if model.lastError != nil {
            return (.systemRed, "Something went wrong (see the window)")
        }
        return (.systemGreen, "Ready to record")
    }

    private var setupTitle: String {
        guard let setup = model.setup, !setup.ready else { return "Settings…" }
        return setup.remaining == 1 ? "Settings… (1 setup step left)" : "Settings… (\(setup.remaining) setup steps left)"
    }

    private func recentTitle(_ s: SessionSummary) -> String {
        let label = s.verdictLabel ?? VerdictBadge(session: s).label
        return [s.title, s.company, label].compactMap { $0 }.joined(separator: " · ")
    }

    private func show(_ window: String) {
        NSApp.activate(ignoringOtherApps: true)
        openWindow(id: window)
    }

    /// A coloured status dot. Menu item images are drawn as templates (grey) unless they opt out.
    private func dot(_ color: NSColor) -> NSImage {
        let image = NSImage(size: NSSize(width: 10, height: 10), flipped: false) { rect in
            color.setFill()
            NSBezierPath(ovalIn: rect.insetBy(dx: 1, dy: 1)).fill()
            return true
        }
        image.isTemplate = false
        return image
    }
}

/// Capture starts only after the user explicitly confirms consent.
struct RecordWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openWindow) private var openWindow
    @State private var consent = false

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 22) {
            HStack(spacing: 14) {
                BrandMark(size: 44)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Record an interview")
                        .font(CoachTheme.editorial(25)).foregroundStyle(CoachTheme.ink)
                    Text("Microphone and call audio, saved together.").font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 14) {
                field("Interview title", text: $model.title, prompt: "e.g. Hiring manager conversation")
                field("Company", text: $model.company, prompt: "Optional")
            }
            HStack(spacing: 14) {
                Label("Your microphone", systemImage: "mic")
                Label("Call audio", systemImage: "waveform")
                Spacer()
                Button("Check audio") { openWindow(id: "setup") }.buttonStyle(.link)
            }
            .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
            VStack(alignment: .leading, spacing: 10) {
                Toggle("Everyone on this call agreed to be recorded", isOn: $consent)
                    .toggleStyle(.checkbox).font(.system(size: 12, weight: .medium))
                Text("Ask before you start. Some places require every participant’s consent.")
                    .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                Text("Audio stays on your Mac. Transcript text goes to your chosen coaching provider, and excerpts go to TypeSafe for evaluation.")
                    .font(.system(size: 10)).foregroundStyle(CoachTheme.muted).lineSpacing(3)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 16).frame(maxWidth: .infinity, alignment: .leading)
            .overlay(alignment: .top) { CoachRule() }
            .overlay(alignment: .bottom) { CoachRule() }
            HStack {
                Text("Stop anytime from the menu bar.").font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button {
                    dismiss()
                    Task { await model.startRecording() }
                } label: { Label("Start recording", systemImage: "record.circle") }
                .buttonStyle(CoachPrimaryButtonStyle())
                .keyboardShortcut(.defaultAction).disabled(!consent || model.phase.isBusy)
            }
        }
        .padding(28).frame(width: 490)
        .background(CoachTheme.canvas).tint(CoachTheme.accent)
        .onAppear { consent = false }
    }

    private func field(_ title: String, text: Binding<String>, prompt: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.system(size: 11, weight: .medium)).foregroundStyle(CoachTheme.ink)
            TextField(title, text: text, prompt: Text(prompt))
                .labelsHidden().textFieldStyle(.roundedBorder).controlSize(.large)
        }
    }
}
