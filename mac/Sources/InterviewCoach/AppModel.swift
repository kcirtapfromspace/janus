import InterviewCoachKit
import AppKit
import AVFoundation
import ICRecorderCore
import Observation

/// App state shared by the menu-bar panel and the main window.
@MainActor @Observable
final class AppModel {
    var appearance: AppAppearance = .system {
        didSet {
            // Fixture renders can switch modes without changing the user's preference.
            if ProcessInfo.processInfo.environment["IC_SNAPSHOTS"] == nil {
                UserDefaults.standard.set(appearance.rawValue, forKey: AppAppearance.defaultsKey)
            }
            appearance.apply()
        }
    }

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
    /// Every interview (archived and deleted ones flagged) and every role: the sidebar.
    var library = Library()
    var filter = LibraryFilter()
    /// The interviews in the main list (not archived or deleted): the menu bar's Recent Interviews.
    var sessions: [SessionSummary] { library.sessions.filter { !$0.archived && !$0.isDeleted } }
    /// The sidebar's selection. One interview shows its stages; several show what you can do with all.
    var listSelection: Set<Int> = [] {
        didSet {
            let one = listSelection.count == 1 ? listSelection.first : nil
            if selection != one { selection = one }
        }
    }
    /// What this Mac still needs (`ic setup status`), shown in the Setup window.
    var setup: SetupStatus?
    /// The setup step running now (one at a time), with its live progress.
    var setupActivity: SetupActivity?
    /// The last failure per setup check, shown on its row until it's retried.
    var setupErrors: [String: String] = [:]
    var selfTest: SelfTestState = .notRun
    /// Models a report can be re-run with (`ic models --json`), loaded when first needed.
    var modelOffers: [ModelOffer] = []
    var modelOffersError: String?
    private var modelOffersAsked: Date?
    var micPermission = AVCaptureDevice.authorizationStatus(for: .audio)
    /// Screen Recording, for the call's video. macOS applies a new grant when the app reopens.
    var screenPermission = ScreenCapture.hasPermission()
    /// Record the call's window along with the audio (the Record window's choice, remembered).
    var recordVideo = UserDefaults.standard.object(forKey: "recordVideo") as? Bool ?? true {
        didSet { UserDefaults.standard.set(recordVideo, forKey: "recordVideo") }
    }
    /// Setup opens by itself at most once per launch.
    @ObservationIgnored var setupPromptShown = false
    var selection: SessionSummary.ID? {
        didSet {
            if let id = selection, listSelection != [id] {
                listSelection = [id]
            } else if selection == nil, listSelection.count == 1 {
                listSelection = []
            }
        }
    }
    var lastError: String?
    var title = ""
    var company = ""
    /// A downloaded, verified update waiting for the app to be idle.
    var updateReady: String?
    /// The selected session's whole pipeline, and which stage the window shows.
    var detail: SessionDetail?
    var detailError: String?
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
        appearance = AppAppearance(rawValue: UserDefaults.standard.string(forKey: AppAppearance.defaultsKey) ?? "") ?? .system
        appearance.apply()
        updater.isIdle = { [unowned self] in phase == .idle }
        updater.onReady = { [unowned self] version in updateReady = version }
        // Snapshot rendering uses fixtures only: no polling, retention cleanup, or updater.
        guard ProcessInfo.processInfo.environment["IC_SNAPSHOTS"] == nil else { return }
        updater.start()
        Task {
            await refresh()
            // Erase what has been in Recently Deleted for 30 days.
            if let ic, (try? await ic.run(["empty-deleted"])) != nil { await refresh() }
        }
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

    var selectedSession: SessionSummary? { library.sessions.first { $0.id == selection } }

    /// Load the selected session's stages, transcript, reports, and next steps.
    func loadDetail() async {
        guard ProcessInfo.processInfo.environment["IC_SNAPSHOTS"] == nil else { return }
        detailError = nil
        guard let ic, let id = selection else {
            detail = nil
            player.load(nil)
            return
        }
        do {
            let loaded = try await ic.decode(SessionDetail.self, ["session", "\(id)"])
            guard selection == id else { return }  // the selection moved on while loading
            if detail?.session.id != id {
                // Open coaching first when available; otherwise show the latest evidence.
                selectedStage = loaded.stage(.report)?.hasResult == true ? .report
                    : loaded.stages.last { $0.hasResult }?.step ?? .recording
            }
            detail = loaded
            player.load(loaded.audio.listenPath, video: loaded.audio.videoPath)
        } catch {
            guard selection == id else { return }
            detailError = error.localizedDescription
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

    /// Load account-specific choices and surface catalog failures in Setup.
    func loadModelOffers(force: Bool = false) async {
        if !force, let asked = modelOffersAsked, Date().timeIntervalSince(asked) < 60 { return }
        modelOffersAsked = Date()
        guard let ic else { return }
        do {
            modelOffers = try await ic.decode([ModelOffer].self, ["models", "--json"])
            modelOffersError = nil
        } catch {
            modelOffers = []
            modelOffersError = error.localizedDescription
        }
    }

    func setDefaultModel(_ value: String) {
        guard let ic, setupActivity == nil, !phase.isBusy else { return }
        setupActivity = SetupActivity(checkID: "model", message: "Saving the coaching model…")
        Task {
            do {
                _ = try await ic.run(["config", "set", "model", value])
                setupErrors["model"] = nil
            } catch { setupErrors["model"] = error.localizedDescription }
            setupActivity = nil
            await refresh()
        }
    }

    /// Save what you saw during one answer as a correction (`ic eval correct`, the labels on stdin).
    func correctVideo(start: Double, correction: VideoCorrection) async -> String? {
        guard let ic, let id = selection else { return "No interview is selected." }
        do {
            _ = try await ic.run(["eval", "correct", "\(id)", "--start", String(start)], stdin: correction.labelsJSON)
            await loadDetail()
            return nil
        } catch {
            return error.localizedDescription
        }
    }

    /// A mock interview ended: review it like any recording, and show it.
    func finishMock(_ id: Int) {
        selection = id
        selectedStage = .report
        runIC(["recording", "finish", "\(id)"], label: "Reviewing your practice interview…", select: id)
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
            library = try await ic.decode(Library.self, ["list", "--json", "--all"])
            forgetVanished()
            setup = try await ic.decode(SetupStatus.self, ["setup", "status", "--json"])
            micPermission = AVCaptureDevice.authorizationStatus(for: .audio)
            screenPermission = ScreenCapture.hasPermission()
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// A background refresh: never shows an error (the next user action will, if it persists).
    private func refreshQuietly(setupToo: Bool) async {
        guard let ic else { return }
        if let loaded = try? await ic.decode(Library.self, ["list", "--json", "--all"]), loaded != library {
            library = loaded
            forgetVanished()
        }
        if setupToo, let status = try? await ic.decode(SetupStatus.self, ["setup", "status", "--json"]), status != setup {
            setup = status
        }
    }

    // MARK: Recording


    func startRecording() async {
        guard let ic, !phase.isBusy else { return }
        // Reserve the recording action before the first await so clicks from another window
        // cannot create a second session while the CLI prepares the first.
        phase = .working("Preparing recording…")
        lastError = nil
        do {
            var args = ["recording", "begin"]
            if !title.isEmpty { args += ["--title", title] }
            if !company.isEmpty { args += ["--company", company] }
            let new = try await ic.decode(NewRecording.self, args)
            let dir = URL(fileURLWithPath: new.dir, isDirectory: true)
            let session = RecordingSession(
                options: RecorderOptions(sessionDir: dir, duration: nil, aec: false, video: recordVideo,
                                         askForScreenPermission: false),
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
            lastError = "Recording could not start or save its result. Any captured audio remains in the session folder; see recorder.log for details."
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
        runIC(["import", url.path], label: "Importing \(url.lastPathComponent)…", selectNew: true)
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

    func signInWithChatGPT(account: String? = nil, newAccount: Bool = false, enablePlan: Bool = false) {
        var args = ["login", "--provider", "openai", "--events"]
        if let account { args += ["--account", account] }
        if newAccount { args += ["--switch"] }
        if enablePlan { args += ["--enable-plan"] }
        follow(args, check: "openai", message: "Approve ChatGPT access in your browser…")
    }

    func signOutOfChatGPT() {
        follow(["logout", "--provider", "openai", "--events"], check: "openai", message: "Signing out of ChatGPT…")
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

    /// Store a provider key privately. It goes to ic on stdin, never as an argument.
    func saveKey(_ key: String, target: String, check: String) async {
        guard let ic, setupActivity == nil else { return }
        setupErrors[check] = nil
        setupActivity = SetupActivity(checkID: check, message: target == "openai" ? "Saving the key and selecting API billing…" : "Saving the TypeSafe key for Jev evaluation…")
        do {
            _ = try await ic.run(["proxy", "key", target, "--stdin"], stdin: key)
        } catch {
            setupErrors[check] = error.localizedDescription
        }
        setupActivity = nil
        await refresh()
        await loadModelOffers(force: true)
    }

    func requestMicAccess() {
        MicCapture.requestAccess { [weak self] _ in
            Task { @MainActor in self?.micPermission = AVCaptureDevice.authorizationStatus(for: .audio) }
        }
    }

    /// Turn a privacy choice on or off (`ic config set`): share-questions, share-company, diagnostics.
    func setPrivacy(_ key: String, _ on: Bool) {
        guard let ic else { return }
        setupErrors["privacy"] = nil
        Task {
            do {
                _ = try await ic.run(["config", "set", key, on ? "on" : "off"])
            } catch {
                setupErrors["privacy"] = error.localizedDescription
            }
            await refresh()
        }
    }

    /// Withdraw everything this Mac shared with the registry.
    func withdrawSharedQuestions() async -> String {
        guard let ic else { return "Couldn't find the ic tool." }
        do {
            return try await ic.run(["registry", "withdraw"]).trimmingCharacters(in: .whitespacesAndNewlines)
                .replacingOccurrences(of: "✓ ", with: "")
        } catch {
            return error.localizedDescription
        }
    }

    func refreshScreenPermission() {
        screenPermission = ScreenCapture.hasPermission()
    }

    /// Asks once with the system prompt; after that, opens the setting. Either way macOS applies
    /// the permission when Janus reopens.
    func requestScreenAccess() {
        if !ScreenCapture.requestPermission() {
            openPrivacySettings("Privacy_ScreenCapture")
        }
        screenPermission = ScreenCapture.hasPermission()
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
                await loadModelOffers(force: true)
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
        case .log, .done, .say:
            break
        }
    }

    func openInBrowser(_ session: SessionSummary) {
        if let path = session.reportPath { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
    }

    func revealInFinder(_ session: SessionSummary) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: session.dir, isDirectory: true)])
    }

    /// Drop selected interviews that no longer exist (erased from Recently Deleted).
    private func forgetVanished() {
        let ids = Set(library.sessions.map(\.id))
        if !listSelection.isSubset(of: ids) { listSelection = listSelection.intersection(ids) }
    }

    // MARK: Managing interviews

    /// Run quick `ic` commands one after another (they don't block recording), then reload.
    private func manage(_ commands: [[String]]) {
        guard let ic else { return }
        lastError = nil
        Task {
            for args in commands {
                do {
                    _ = try await ic.run(args)
                } catch {
                    lastError = error.localizedDescription
                    break
                }
            }
            await refresh()
            await loadDetail()
        }
    }

    func archive(_ ids: [Int], undo: Bool = false) { manage([["archive"] + ids.map(String.init) + (undo ? ["--undo"] : [])]) }
    func delete(_ ids: [Int]) { manage([["delete"] + ids.map(String.init)]) }
    func restore(_ ids: [Int]) { manage([["restore"] + ids.map(String.init)]) }
    /// Erase interviews in Recently Deleted for good.
    func erase(_ ids: [Int]) { manage([["erase"] + ids.map(String.init)]) }
    func emptyRecentlyDeleted() { manage([["empty-deleted", "--now"]]) }

    /// Move interviews to a role (by id), a new role (by title) or out of their role (both nil).
    func move(_ ids: [Int], toRole roleID: Int? = nil, newRole title: String? = nil) {
        let target: [String] = if let roleID { ["--role-id", "\(roleID)"] } else if let title { ["--role", title] } else { ["--no-role"] }
        manage(ids.map { ["edit", "\($0)"] + target })
    }

    /// Save the Edit Details sheet: only what changed (nil = unchanged). The role is a role id, a
    /// new role's title, or neither (out of its role).
    func editDetails(_ id: Int, title: String?, company: String?, role: (id: Int?, newTitle: String?)?, round: String??) {
        var commands: [[String]] = []
        var first = ["edit", "\(id)"]
        if let title { first += ["--title", title] }
        if let company { first += company.trimmingCharacters(in: .whitespaces).isEmpty ? ["--no-company"] : ["--company", company] }
        if let round { first += ["--round", round ?? "none"] }
        if first.count > 2 { commands.append(first) }
        if let role {
            if let newTitle = role.newTitle?.trimmingCharacters(in: .whitespaces), !newTitle.isEmpty {
                commands.append(["edit", "\(id)", "--role", newTitle])
            } else if let roleID = role.id {
                commands.append(["edit", "\(id)", "--role-id", "\(roleID)"])
            } else {
                commands.append(["edit", "\(id)", "--no-role"])
            }
        }
        if !commands.isEmpty { manage(commands) }
    }

    func setRoleStatus(_ roleID: Int, _ status: String) { manage([["role", "status", "\(roleID)", status]]) }
    func renameRole(_ roleID: Int, _ title: String) { manage([["role", "rename", "\(roleID)", title]]) }
    func archiveRole(_ roleID: Int, undo: Bool = false) { manage([["role", "archive", "\(roleID)"] + (undo ? ["--undo"] : [])]) }
    func mergeRole(_ from: Int, into: Int) { manage([["role", "merge", "\(from)", "\(into)"]]) }
    func renameCompany(_ old: String, _ new: String) { manage([["company", "rename", old, new]]) }
    func archiveCompany(_ name: String, undo: Bool = false) { manage([["company", "archive", name] + (undo ? ["--undo"] : [])]) }

    /// Interviews whose transcript mentions the search text (titles and companies match in the sidebar itself).
    func searchTranscripts(_ query: String) async {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard let ic, q.count >= 2 else {
            filter.searchHits = []
            return
        }
        let hits = (try? await ic.decode([SearchHit].self, ["search", q, "--json"])) ?? []
        guard filter.query.trimmingCharacters(in: .whitespaces) == q else { return }
        filter.searchHits = Set(hits.map(\.sessionId))
    }

    private func runIC(_ args: [String], label: String, select id: Int? = nil, selectNew: Bool = false) {
        guard let ic else { return }
        lastError = nil
        phase = .working(label)
        let previousIDs = Set(library.sessions.map(\.id))
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
            else if selectNew, let imported = sessions.filter({ !previousIDs.contains($0.id) }).max(by: { $0.id < $1.id }) {
                selection = imported.id
            }
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
