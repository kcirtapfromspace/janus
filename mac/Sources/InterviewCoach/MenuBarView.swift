import InterviewCoachKit
import SwiftUI

/// The panel that drops down from the menu-bar icon: record/stop, recent interviews, status.
struct MenuBarView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Interview Coach").font(.headline)

            if let setup = model.setup, !setup.ready {
                Button { showSetup() } label: {
                    Label(setup.remaining == 1 ? "Finish setup (1 thing left)…" : "Finish setup (\(setup.remaining) things left)…",
                          systemImage: "wrench.and.screwdriver")
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.borderedProminent)
                .tint(.orange)
            }
            if let version = model.updateReady, model.phase.isBusy {
                Label("Version \(version) installs when this finishes", systemImage: "arrow.down.circle")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            controls

            if let error = model.lastError {
                Text(error)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }

            Divider()
            recent
            Divider()

            HStack {
                Button("Open Interview Coach") { showMainWindow() }
                Spacer()
                Button("Setup…") { showSetup() }
                if model.updater.isAvailable {
                    Button("Check for Updates…") { model.updater.checkForUpdates() }
                        .disabled(model.phase.isRecording)
                }
                Button("Import…") { model.importRecording() }
                    .disabled(model.phase.isBusy)
                Button("Quit") { NSApp.terminate(nil) }
                    .disabled(model.phase.isRecording)
                    .help(model.phase.isRecording ? "Stop the recording first" : "")
            }
            .controlSize(.small)
        }
        .padding(14)
        .frame(width: 330)
        .task { await model.refresh() }
    }

    @ViewBuilder private var controls: some View {
        @Bindable var model = model
        switch model.phase {
        case .idle:
            VStack(spacing: 8) {
                TextField("Title (optional)", text: $model.title)
                TextField("Company (optional)", text: $model.company)
                Button { model.askConsent() } label: {
                    Label("Record interview", systemImage: "record.circle")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(.red)
                .controlSize(.large)
            }
            .textFieldStyle(.roundedBorder)

        case .confirmingConsent:
            VStack(alignment: .leading, spacing: 8) {
                Text("Has everyone on the call agreed to be recorded?")
                    .font(.callout.weight(.semibold))
                Text("Some places require every participant's consent.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                HStack {
                    Button("Cancel") { model.cancelConsent() }
                    Spacer()
                    Button("Yes — start recording") { Task { await model.startRecording() } }
                        .buttonStyle(.borderedProminent)
                        .tint(.red)
                }
            }

        case .recording(let since):
            HStack {
                Image(systemName: "record.circle.fill")
                    .foregroundStyle(.red)
                    .symbolEffect(.pulse)
                Text("Recording")
                ElapsedTime(since: since)
                    .foregroundStyle(.secondary)
                Spacer()
                Button { model.stopRecording() } label: {
                    Label("Stop", systemImage: "stop.fill")
                }
                .buttonStyle(.borderedProminent)
            }
            if let problem = model.captureProblem {
                ProblemRow(problem: problem)
            }

        case .working(let label):
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(label).font(.callout)
            }
        }
    }

    @ViewBuilder private var recent: some View {
        Text("Recent").font(.caption).foregroundStyle(.secondary)
        if model.sessions.isEmpty {
            Text("No interviews yet.")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
        ForEach(model.sessions.prefix(5)) { session in
            Button {
                model.selection = session.id
                showMainWindow()
            } label: {
                SessionRow(session: session)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
    }

    private func showMainWindow() {
        openWindow(id: "main")
        NSApp.activate(ignoringOtherApps: true)
    }

    private func showSetup() {
        NSApp.activate(ignoringOtherApps: true)
        openWindow(id: "setup")
    }
}
