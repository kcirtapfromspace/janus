import InterviewCoachKit
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
    /// The selected session's whole pipeline, and which stage the window shows.
    var detail: SessionDetail?
    var selectedStage: StageStep = .report
    let player = AudioPlayer()
    /// While recording: a track that has stopped receiving audio (e.g. a call app took the mic).
    var captureProblem: String?

    let ic = ICClient.locate()
    let updater = Updater()
    private var recorder: RecordingSession?
    private var recordingID: Int?
    @ObservationIgnored private var healthTimer: Timer?

    init() {
        updater.isIdle = { [unowned self] in phase == .idle }
        updater.onReady = { [unowned self] version in updateReady = version }
        updater.start()
        Task { await refresh() }
    }

    var selectedSession: SessionSummary? { sessions.first { $0.id == selection } }

    /// Load the selected session's stages, transcript, reports, and next steps.
    func loadDetail() async {
        guard let ic, let id = selection else {
            detail = nil
            return
        }
        do {
            let loaded = try await ic.decode(SessionDetail.self, ["session", "\(id)"])
            guard selection == id else { return }  // the selection moved on while loading
            if detail?.session.id != id {
                // A different interview: start on its furthest stage that has something to show.
                selectedStage = loaded.stages.last { $0.hasResult }?.step ?? .recording
            }
            detail = loaded
            player.load(loaded.audio.listenPath)
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Re-run one stage; later stages built on it go out of date (or update too, with `thenLater`).
    func rerun(_ step: StageStep, options: [String] = [], thenLater: Bool = false) {
        guard let id = selection else { return }
        selectedStage = step
        let label = detail?.stage(step)?.label.lowercased() ?? step.rawValue
        runIC(["run", step.rawValue, "\(id)"] + options + (thenLater ? ["--then-later"] : []),
              label: "Running the \(label)…", select: id)
    }

    /// Bring every out-of-date stage current, starting from the first.
    func updateLaterSteps() {
        guard let stale = detail?.firstOutOfDate else { return }
        rerun(stale.step, thenLater: true)
    }

    func swapSpeakers() {
        guard let id = selection else { return }
        selectedStage = .transcript
        runIC(["swap", "\(id)"], label: "Swapping speakers…", select: id)
    }

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
            selection = new.id  // the window follows the interview through its stages
            phase = .recording(since: Date())
            session.start()  // asks for microphone permission on first use
            watchCaptureHealth()
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

    /// Checks every second that both tracks are still receiving audio, so a dead mic shows up
    /// during the interview rather than in the report afterwards.
    private func watchCaptureHealth() {
        healthTimer?.invalidate()
        let timer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let problem = self.recorder?.health()?.problem
                if problem != self.captureProblem { self.captureProblem = problem }
            }
        }
        RunLoop.main.add(timer, forMode: .common)  // keeps checking while a menu is open
        healthTimer = timer
    }

    private func recordingEnded(exitCode: Int32) {
        healthTimer?.invalidate()
        healthTimer = nil
        captureProblem = nil
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
        selection = id
        rerun(.report)
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
        // While ic works, keep the pipeline view live: stage status and progress come from the database.
        let poll = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self, !Task.isCancelled else { return }
                if id == nil || selection == id { await loadDetail() }
            }
        }
        Task {
            do {
                _ = try await ic.run(args)
            } catch {
                lastError = error.localizedDescription
            }
            poll.cancel()
            await refresh()
            if let id { selection = id }
            await loadDetail()
            phase = .idle
        }
    }
}
