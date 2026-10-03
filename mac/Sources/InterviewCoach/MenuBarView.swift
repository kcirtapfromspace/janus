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
        Button { show("main") } label: { Label("Open Interview Coach", systemImage: "macwindow") }
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
        if model.updater.isAvailable {
            Button { model.updater.checkForUpdates() } label: {
                Label(model.updateReady.map { "Update to \($0) Ready" } ?? "Check for Updates…",
                      systemImage: model.updateReady == nil ? "arrow.triangle.2.circlepath" : "arrow.down.circle")
            }
            .disabled(model.phase.isRecording)
        }
        Button("About Interview Coach") {
            NSApp.activate(ignoringOtherApps: true)
            NSApp.orderFrontStandardAboutPanel(nil)
        }

        Divider()
        Button("Quit Interview Coach") { NSApp.terminate(nil) }
            .keyboardShortcut("q")
            .disabled(model.phase.isRecording)
    }

    /// The first line: a coloured dot and what Interview Coach is doing (not clickable).
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
        guard let setup = model.setup, !setup.ready else { return "Setup…" }
        return setup.remaining == 1 ? "Setup… (1 thing left)" : "Setup… (\(setup.remaining) things left)"
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

/// "Record Interview…": an optional title and company, and the consent confirmation, before
/// recording starts. The same window serves the menu and the main window's Record button.
struct RecordWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        @Bindable var model = model
        VStack(alignment: .leading, spacing: 14) {
            Text("Record an interview").font(.title3.weight(.semibold))
            Form {
                TextField("Title", text: $model.title, prompt: Text("Optional, e.g. Hiring manager round"))
                TextField("Company", text: $model.company, prompt: Text("Optional"))
            }
            .formStyle(.columns)
            Label {
                Text("Has everyone on the call agreed to be recorded? Some places require every participant's consent.")
                    .fixedSize(horizontal: false, vertical: true)
            } icon: {
                Image(systemName: "person.2.wave.2").foregroundStyle(.secondary)
            }
            .font(.callout)
            Text("Records your mic and the call's audio from any app until you choose Stop Recording.")
                .font(.caption)
                .foregroundStyle(.secondary)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button("Everyone Agreed — Start Recording") {
                    dismiss()
                    Task { await model.startRecording() }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(model.phase.isBusy)
            }
        }
        .padding(20)
        .frame(width: 440)
    }
}
