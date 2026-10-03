import InterviewCoachKit
import AppKit
import AVFoundation
import ICRecorderCore
import Observation

/// App state shared by the menu-bar panel and the main window.
@MainActor @Observable
final class AppModel {
    enum Phase: Equatable {
        case idle
        case recording(since: Date)
        case working(String)  // a long `ic` step: transcribing, analysing, importing

        var isBusy: Bool { self != .idle }
        var isRecording: Bool { if case .recording = self { true } else { false } }
    }

    var phase: Phase = .idle {
        didSet { if phase == .idle { updater.installIfIdle() } }  // a waiting update installs between interviews
    }
    var sessions: [SessionSummary] = []
    /// What this Mac still needs (`ic setup status`), shown in the Setup window.
    var setup: SetupStatus?
    /// The setup step running now (one at a time), with its live progress.
    var setupActivity: SetupActivity?
    /// The last failure per setup check, shown on its row until it's retried.
    var setupErrors: [String: String] = [:]
    var selfTest: SelfTestState = .notRun
    /// Models a report can be re-run with (`ic models --json`), loaded when first needed.
    var modelOffers: [ModelOffer] = []
    private var modelOffersAsked: Date?
    var micPermission = AVCaptureDevice.authorizationStatus(for: .audio)
    /// Setup opens by itself at most once per launch.
    @ObservationIgnored var setupPromptShown = false
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
    @ObservationIgnored private var setupStream: ICStream?
    @ObservationIgnored private var setupCancelled = false
    @ObservationIgnored private var runningSelfTest: AudioSelfTest?

    init() {
        updater.isIdle = { [unowned self] in phase == .idle }
        updater.onReady = { [unowned self] version in updateReady = version }
        updater.start()
        Task { await refresh() }
        // The menu is native, so there's no "menu opened" moment to refresh on: keep the interview
        // list current every 10 s (a quick local read) and setup every minute (it checks Docker,
        // the proxy and the sign-in).
        Task { [weak self] in
            var tick = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                guard let self else { return }
                tick += 1
                await self.refreshQuietly(setupToo: tick % 6 == 0)
            }
        }
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

    /// The models a report can be written with, and their prices. Quiet on failure: the menu then
    /// offers the configured default and "cheapest", which ic resolves itself.
    func loadModelOffers() async {
        // At most once a minute while it keeps failing (e.g. the proxy isn't running yet).
        if let asked = modelOffersAsked, Date().timeIntervalSince(asked) < 60 { return }
        modelOffersAsked = Date()
        guard let ic, let offers = try? await ic.decode([ModelOffer].self, ["models", "--json"]) else { return }
        modelOffers = offers
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
            setup = try await ic.decode(SetupStatus.self, ["setup", "status", "--json"])
            micPermission = AVCaptureDevice.authorizationStatus(for: .audio)
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// A background refresh: never shows an error (the next user action will, if it persists).
    private func refreshQuietly(setupToo: Bool) async {
        guard let ic else { return }
        if let list = try? await ic.decode([SessionSummary].self, ["list", "--json"]), list != sessions {
            sessions = list
        }
        if setupToo, let status = try? await ic.decode(SetupStatus.self, ["setup", "status", "--json"]), status != setup {
            setup = status
        }
    }

    // MARK: Recording


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

    // MARK: Setup

    /// Run one `ic setup` step, following its progress.
    func runSetupStep(_ step: String, check: String) {
        follow(["setup", "run", step, "--events"], check: check, message: "Starting…")
    }

    /// Browser sign-in: ic runs the bundled `ant`, which opens the approval page. `switching` signs
    /// out first, to sign in with a different account.
    func signIn(switching: Bool = false) {
        follow(["login", "--events"] + (switching ? ["--switch"] : []), check: "claude",
               message: switching ? "Signing out…" : "Approve access in your browser…")
    }

    /// The code the sign-in page shows when it can't hand the approval back by itself.
    func sendSignInCode(_ code: String) {
        let code = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !code.isEmpty else { return }
        setupStream?.send(code)
        setupActivity?.needsCode = false
        setupActivity?.message = "Checking the code…"
    }

    func cancelSetupActivity() {
        setupCancelled = true
        setupStream?.cancel()
    }

    /// Store an API key in the AI proxy. It goes to ic on stdin, never as an argument.
    func saveKey(_ key: String, target: String, check: String) async {
        guard let ic, setupActivity == nil else { return }
        setupErrors[check] = nil
        setupActivity = SetupActivity(checkID: check, message: "Saving the key and restarting the AI proxy…")
        do {
            _ = try await ic.run(["proxy", "key", target, "--stdin"], stdin: key)
        } catch {
            setupErrors[check] = error.localizedDescription
        }
        setupActivity = nil
        await refresh()
    }

    func requestMicAccess() {
        MicCapture.requestAccess { [weak self] _ in
            Task { @MainActor in self?.micPermission = AVCaptureDevice.authorizationStatus(for: .audio) }
        }
    }

    /// Record 5 s from both tracks to prove the permissions work. Only ever started by a click.
    func runSelfTest() {
        guard runningSelfTest == nil, !phase.isRecording else { return }
        selfTest = .running
        let test = AudioSelfTest()
        runningSelfTest = test
        test.run { [weak self] result in
            Task { @MainActor in
                guard let self else { return }
                self.runningSelfTest = nil
                self.selfTest = .finished(result)
                self.micPermission = AVCaptureDevice.authorizationStatus(for: .audio)
            }
        }
    }

    private func follow(_ args: [String], check: String, message: String) {
        guard let ic, setupActivity == nil else { return }
        setupErrors[check] = nil
        setupCancelled = false
        setupActivity = SetupActivity(checkID: check, message: message)
        do {
            let stream = try ic.stream(args) { [weak self] event in self?.handle(event) }
            setupStream = stream
            Task {
                do {
                    try await stream.wait()
                } catch where !setupCancelled {
                    setupErrors[check] = error.localizedDescription
                } catch {}
                setupStream = nil
                setupActivity = nil
                await refresh()
            }
        } catch {
            setupErrors[check] = error.localizedDescription
            setupActivity = nil
        }
    }

    private func handle(_ event: SetupEvent) {
        switch event {
        case .stage(let message):
            setupActivity?.message = message
            setupActivity?.progress = nil
        case .progress(let done, let total):
            setupActivity?.progress = total > 0 ? Double(done) / Double(total) : nil
            setupActivity?.detail = total > 10_000_000
                ? "\(ByteCountFormatter.string(fromByteCount: done, countStyle: .file)) of \(ByteCountFormatter.string(fromByteCount: total, countStyle: .file))"
                : nil
        case .openURL(let url):
            setupActivity?.signInURL = url
        case .needCode:
            setupActivity?.needsCode = true
            setupActivity?.message = "Paste the code the page shows."
        case .error(let message):
            if let check = setupActivity?.checkID { setupErrors[check] = message }
        case .log, .done:
            break
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

/// A setup step in progress.
struct SetupActivity: Equatable {
    let checkID: String
    var message: String
    var progress: Double?
    var detail: String?
    var signInURL: URL?
    var needsCode = false
}

enum SelfTestState: Equatable {
    case notRun
    case running
    case finished(AudioSelfTest.Result)
}
