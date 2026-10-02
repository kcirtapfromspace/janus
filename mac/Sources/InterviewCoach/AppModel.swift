import AppKit
import ICRecorderCore
import Observation

/// App state shared by the menu-bar panel and the main window.
@MainActor @Observable
final class AppModel {
    enum Phase: Equatable {
        case idle
        case confirmingConsent
        case recording(since: Date)
        case working(String)  // a long `ic` step: transcribing, analysing, importing

        var isBusy: Bool { self != .idle && self != .confirmingConsent }
        var isRecording: Bool { if case .recording = self { true } else { false } }
    }

    var phase: Phase = .idle {
        didSet { if phase == .idle { updater.installIfIdle() } }  // a waiting update installs between interviews
    }
    var sessions: [SessionSummary] = []
    var health: Health?
    var selection: SessionSummary.ID?
    var lastError: String?
    var title = ""
    var company = ""
    /// A downloaded, verified update waiting for the app to be idle.
    var updateReady: String?

    let ic = ICClient.locate()
    let updater = Updater()
    private var recorder: RecordingSession?
    private var recordingID: Int?

    init() {
        updater.isIdle = { [unowned self] in phase == .idle }
        updater.onReady = { [unowned self] version in updateReady = version }
        updater.start()
        Task { await refresh() }
    }

    var selectedSession: SessionSummary? { sessions.first { $0.id == selection } }

    func refresh() async {
        guard let ic else {
            lastError = "Couldn't find the `ic` tool. Rebuild the app with mac/build.sh."
            return
        }
        do {
            sessions = try await ic.decode([SessionSummary].self, ["list", "--json"])
            health = try await ic.decode(Health.self, ["doctor", "--json"])
        } catch {
            lastError = error.localizedDescription
        }
    }

    // MARK: Recording

    /// Recording needs a "yes, everyone agreed" first — asked inline, not in a modal.
    func askConsent() {
        lastError = nil
        phase = .confirmingConsent
    }

    func cancelConsent() {
        phase = .idle
    }

    func startRecording() async {
        guard let ic else { return }
        do {
            var args = ["recording", "begin"]
            if !title.isEmpty { args += ["--title", title] }
            if !company.isEmpty { args += ["--company", company] }
            let new = try await ic.decode(NewRecording.self, args)
            let dir = URL(fileURLWithPath: new.dir, isDirectory: true)
            let session = RecordingSession(
                options: RecorderOptions(sessionDir: dir, duration: nil, aec: false),
                log: Logger(fileURL: dir.appendingPathComponent("recorder.log")),
                onExit: { [weak self] code in
                    Task { @MainActor in self?.recordingEnded(exitCode: code) }
                }
            )
            recorder = session
            recordingID = new.id
            phase = .recording(since: Date())
            session.start()  // asks for microphone permission on first use
            title = ""
            company = ""
            await refresh()
        } catch {
            phase = .idle
            lastError = error.localizedDescription
        }
    }

    func stopRecording() {
        recorder?.stop(reason: "stopped in app")  // finalizes the WAVs, then calls onExit
    }

    private func recordingEnded(exitCode: Int32) {
        recorder = nil
        guard let id = recordingID else { return }
        recordingID = nil
        if exitCode == 2 {
            phase = .idle
            lastError = "Couldn't start recording — see recorder.log in the session folder."
            return
        }
        // Exit code 1 (e.g. permission denied) still leaves recorder.json; ic reports the problem.
        runIC(["recording", "finish", "\(id)"], label: "Transcribing and analysing…", select: id)
    }

    // MARK: Other actions

    func importRecording() {
        NSApp.activate(ignoringOtherApps: true)
        let panel = NSOpenPanel()
        panel.title = "Import an interview recording"
        panel.allowedContentTypes = [.audio, .movie, .audiovisualContent]
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        runIC(["import", url.path], label: "Importing \(url.lastPathComponent)…")
    }

    func analyze(_ id: Int) {
        runIC(["analyze", "\(id)"], label: "Analysing…", select: id)
    }

    func setOutcome(_ id: Int, _ outcome: String) {
        runIC(["outcome", "\(id)", outcome], label: "Saving outcome…", select: id)
    }

    /// `ic login` opens the browser and may ask to paste a code, so it runs in a Terminal window.
    func signInInTerminal() {
        guard let ic else { return }
        let script = FileManager.default.temporaryDirectory.appendingPathComponent("interview-coach-sign-in.command")
        let body = """
        #!/bin/zsh
        export PATH="/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        clear
        '\(ic.executable.path)' login
        echo
        echo "Done. You can close this window and go back to Interview Coach."
        """
        do {
            try body.write(to: script, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: script.path)
            NSWorkspace.shared.open(script)
        } catch {
            lastError = "Couldn't open Terminal: \(error.localizedDescription)"
        }
    }

    func openInBrowser(_ session: SessionSummary) {
        if let path = session.reportPath { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
    }

    func revealInFinder(_ session: SessionSummary) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: session.dir, isDirectory: true)])
    }

    private func runIC(_ args: [String], label: String, select id: Int? = nil) {
        guard let ic else { return }
        lastError = nil
        phase = .working(label)
        Task {
            do {
                _ = try await ic.run(args)
            } catch {
                lastError = error.localizedDescription
            }
            await refresh()
            if let id { selection = id }
            phase = .idle
        }
    }
}
