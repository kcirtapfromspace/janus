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
        if let problem = model.captureProblem {
            Button("View recording warning…") { show("main") }.help(problem)
        }
        Divider()

        if case .recording = model.phase {
            Button { model.stopRecording() } label: { Label("Stop Recording", systemImage: "stop.circle") }
                .keyboardShortcut(".")
        } else {
            Button { show("record") } label: { Label("Record Interview…", systemImage: "record.circle") }
                .keyboardShortcut("r")
                .disabled(model.phase.isBusy)
        }
        Button { show("mock") } label: { Label("Practice Interview…", systemImage: "person.wave.2") }
            .keyboardShortcut("p")
            .disabled(model.phase.isBusy)
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
            AboutJanus.show()
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
            return (.systemOrange, "Recording needs attention")
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
                    Text(model.recordVideo ? "Microphone, call audio and your selected video source, saved together."
                                           : "Microphone and call audio, saved together.")
                        .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                }
            }
            VStack(alignment: .leading, spacing: 14) {
                field("Interview title", text: $model.title, prompt: "e.g. Hiring manager conversation")
                if let role = model.recordRole {
                    // A new round of a role: it takes the role's company.
                    VStack(alignment: .leading, spacing: 7) {
                        Text("Adds a round to").font(.system(size: 11, weight: .medium)).foregroundStyle(CoachTheme.ink)
                        HStack(spacing: 8) {
                            Image(systemName: "briefcase").foregroundStyle(CoachTheme.muted)
                            Text([role.title, role.company].compactMap { $0 }.joined(separator: " · "))
                                .font(.system(size: 12)).foregroundStyle(CoachTheme.ink).lineLimit(1)
                            Spacer()
                            Button("Don’t add to a role") { model.recordRole = nil }
                            .buttonStyle(.link).font(.system(size: 11))
                            .disabled(model.phase.isBusy)
                        }
                    }
                } else {
                    field("Company", text: $model.company, prompt: "Optional")
                }
            }
            HStack(spacing: 14) {
                Label("Your microphone", systemImage: "mic")
                Label("Call audio", systemImage: "waveform")
                if model.recordVideo { Label("Call video", systemImage: "video") }
                Spacer()
                Button("Check audio") { openWindow(id: "setup") }.buttonStyle(.link)
            }
            .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
            VStack(alignment: .leading, spacing: 6) {
                Toggle("Also record video", isOn: $model.recordVideo)
                    .toggleStyle(.checkbox).font(.system(size: 12))
                if model.recordVideo {
                    HStack(spacing: 10) {
                        Text(model.selectedVideoLabel ?? "Automatic meeting window")
                            .lineLimit(2).font(.system(size: 11)).foregroundStyle(CoachTheme.ink)
                        Spacer()
                        Button(model.isPickingVideoSource ? "Choosing…" : "Choose source…") { model.chooseVideoSource() }
                            .disabled(model.isPickingVideoSource || model.phase.isBusy)
                    }
                    Text("Choose a window, app or whole screen. For one browser tab, move it into its own window and select that window.")
                        .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                        .fixedSize(horizontal: false, vertical: true)
                    if model.selectedVideoLabel != nil {
                        if model.selectedVideoIsScreen {
                            Text("This records everything visible on the selected screen, including other apps and notifications.")
                                .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        Button("Use automatic meeting detection") { model.resetVideoSource() }
                            .buttonStyle(.link).font(.system(size: 11))
                    } else {
                        Toggle("If no meeting is detected, record my main screen", isOn: $model.fallbackToScreen)
                            .toggleStyle(.checkbox).font(.system(size: 11))
                        if model.fallbackToScreen {
                            Text("The fallback records everything visible on your main screen, including other apps and notifications.")
                                .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                    if !model.videoSourceReady {
                        Text("Choose a source above, or allow Screen Recording for automatic detection.")
                            .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                        HStack(spacing: 10) {
                            Button("Allow Screen Recording…") { model.requestScreenAccess() }
                            Text("Then reopen Janus.").font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                        }
                    }
                    if let error = model.videoSourceError {
                        Text(error).font(.system(size: 11)).foregroundStyle(CoachTheme.alert)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
            .disabled(model.phase.isBusy)
            VStack(alignment: .leading, spacing: 10) {
                Toggle(model.recordVideo ? "Everyone on this call agreed to be recorded, including video"
                                         : "Everyone on this call agreed to be recorded", isOn: $consent)
                    .toggleStyle(.checkbox).font(.system(size: 12, weight: .medium))
                Text("Ask before you start. Some places require every participant’s consent.")
                    .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                Text(model.recordVideo
                     ? "Audio and video stay on your Mac; faces in the video are read on this Mac and never sent. Transcript text goes to your chosen coaching provider, and excerpts go to TypeSafe for evaluation."
                     : "Audio stays on your Mac. Transcript text goes to your chosen coaching provider, and excerpts go to TypeSafe for evaluation.")
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
                    .disabled(model.phase.isBusy)
                Button {
                    Task {
                        await model.startRecording()
                        if model.phase.isRecording { dismiss() }
                    }
                } label: { Label("Start recording", systemImage: "record.circle") }
                .buttonStyle(CoachPrimaryButtonStyle())
                .keyboardShortcut(.defaultAction)
                .disabled(!consent || model.phase.isBusy || model.isPickingVideoSource || (model.recordVideo && !model.videoSourceReady))
            }
        }
        .padding(28).frame(width: 490)
        .background(CoachTheme.canvas).tint(CoachTheme.accent)
        .onAppear {
            consent = false
            if ProcessInfo.processInfo.environment["IC_SNAPSHOTS"] == nil {
                if !model.phase.isBusy { model.resetVideoSource() }
                model.refreshScreenPermission()
            }
        }
        .onDisappear {
            if !model.phase.isBusy, ProcessInfo.processInfo.environment["IC_SNAPSHOTS"] == nil {
                model.resetVideoSource()
                model.recordRole = nil  // closed without recording: the next one isn't that role's round
            }
        }
        .onChange(of: model.recordVideo) { consent = false }  // they agreed to something else
        .onChange(of: model.videoSelectionRevision) { consent = false }
        .onChange(of: model.fallbackToScreen) { consent = false }
    }

    private func field(_ title: String, text: Binding<String>, prompt: String) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Text(title).font(.system(size: 11, weight: .medium)).foregroundStyle(CoachTheme.ink)
            TextField(title, text: text, prompt: Text(prompt))
                .labelsHidden().textFieldStyle(.roundedBorder).controlSize(.large)
        }
    }
}
