import AVFoundation
import ICRecorderCore
import InterviewCoachKit
import SwiftUI

/// Everything a Mac needs before Janus works, one row each, with the button that fixes
/// it. Opens by itself at launch while anything required is missing; also under "Setup…".
struct SetupView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    appearanceChoice
                    Divider()
                    if let setup = model.setup {
                        modelChoice(setup)
                        Divider()
                        ForEach(setup.checks) { check in
                            CheckRow(check: check)
                            Divider()
                        }
                    } else {
                        ProgressView("Checking this Mac…")
                            .frame(maxWidth: .infinity)
                            .padding(30)
                    }
                    MicrophoneRow()
                    Divider()
                    ScreenRecordingRow()
                    Divider()
                    SharingRow()
                    Divider()
                    DiagnosticsRow()
                    Divider()
                    RecordingTestRow()
                }
            }
        }
        .frame(width: 560)
        .frame(minHeight: 420, idealHeight: 640)
        .refreshWhenShown { await model.refresh() }
        .task { await model.loadModelOffers() }
    }

    private var appearanceChoice: some View {
        @Bindable var model = model
        return VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Appearance").font(.body.weight(.medium))
                Spacer()
                Picker("Appearance", selection: $model.appearance) {
                    ForEach(AppAppearance.allCases) { appearance in
                        Text(appearance.title).tag(appearance)
                    }
                }
                .pickerStyle(.segmented).labelsHidden().frame(width: 230)
            }
            Text("System follows your Mac’s light or dark appearance.")
                .font(.callout).foregroundStyle(.secondary)
        }
        .padding(18)
    }

    private func modelChoice(_ setup: SetupStatus) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("Coaching model").font(.body.weight(.medium))
                Spacer()
                Button("Refresh models") { Task { await model.loadModelOffers(force: true) } }
                    .disabled(model.setupActivity != nil)
            }
            Picker("Default model", selection: Binding(get: { setup.model }, set: { model.setDefaultModel($0) })) {
                if !model.modelOffers.contains(where: { $0.model == setup.model }) {
                    Text("\(setup.model) (current)").tag(setup.model)
                }
                ForEach(model.modelOffers) { offer in
                    Text("\(offer.provider == "openai" ? "OpenAI" : "Claude") · \(offer.name)").tag(offer.model)
                }
            }
            .disabled(model.setupActivity != nil || model.phase.isBusy)
            Text("Sign in, refresh the available models, then choose one for new reports. Earlier reports keep their model and version.")
                .font(.callout).foregroundStyle(.secondary)
            if let error = model.modelOffersError ?? model.setupErrors["model"] {
                Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled)
            }
        }
        .padding(18)
    }

    private var header: some View {
        HStack(alignment: .center, spacing: 14) {
            BrandMark(size: 42)
            VStack(alignment: .leading, spacing: 3) {
                Text("Settings").font(CoachTheme.editorial(25)).foregroundStyle(CoachTheme.ink)
                Text(summary).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Check again") { Task { await model.refresh() } }
                .disabled(model.setupActivity != nil)
        }
        .padding(22)
        .background(CoachTheme.canvas)
    }

    private var summary: String {
        guard let setup = model.setup else { return "Looking at what's installed…" }
        if setup.ready { return "Ready to record and analyse interviews." }
        return setup.remaining == 1 ? "1 thing left to do." : "\(setup.remaining) things left to do."
    }
}

private struct CheckRow: View {
    @Environment(AppModel.self) private var model
    let check: SetupCheck
    @State private var enteringKey = false
    @State private var key = ""
    @State private var code = ""
    @State private var confirmingSwitch = false

    private var activity: SetupActivity? {
        model.setupActivity.flatMap { $0.checkID == check.id ? $0 : nil }
    }

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: check.status, running: activity != nil)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Text(check.title).font(.body.weight(.medium))
                if !check.detail.isEmpty {
                    Text(check.detail).font(.callout).foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if check.id == "openai", let account = model.setup?.openai {
                    HStack {
                        Menu("Accounts") {
                            ForEach(account.accounts) { saved in
                                Button(saved.label) { model.signInWithChatGPT(account: saved.clientId) }
                            }
                            Button("Add ChatGPT account…") { model.signInWithChatGPT(newAccount: true) }
                            if account.signedIn { Button("Sign out of ChatGPT") { model.signOutOfChatGPT() } }
                        }
                        .disabled(model.setupActivity != nil || model.phase.isBusy)
                        Link("Manage ChatGPT usage", destination: URL(string: "https://chatgpt.com/settings/usage")!)
                    }
                    .font(.callout)
                }
                if let activity { ActivityView(activity: activity, code: $code) }
                if enteringKey { keyEntry }
                if let error = model.setupErrors[check.id] {
                    Label(error, systemImage: "xmark.octagon.fill")
                        .font(.callout)
                        .foregroundStyle(.red)
                        .textSelection(.enabled)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer(minLength: 8)
            actionButton
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
        .confirmationDialog("Switch Claude account?", isPresented: $confirmingSwitch) {
            Button("Sign Out and Switch") {
                code = ""
                model.signIn(switching: true)
            }
        } message: {
            Text("You'll be signed out here, then asked to approve access in your browser, where you choose the "
                 + "account and organization. If it picks the wrong account, switch accounts at claude.ai first. "
                 + "Other apps keep their own sign-in.")
        }
    }

    @ViewBuilder private var actionButton: some View {
        if activity != nil {
            Button("Cancel") { model.cancelSetupActivity() }
        } else if let action = check.action, check.status != .blocked, !enteringKey {
            // Signing out or restarting the proxy mid-analysis would fail that step, so these wait.
            let waits = interruptsWork(action) && { if case .working = model.phase { true } else { false } }()
            let button = Button(action.label) { perform(action) }
                .disabled(model.setupActivity != nil || waits)
                .help(waits ? "Available when the current step finishes." : "")
            if check.status == .action { button.buttonStyle(.borderedProminent) } else { button }
        }
    }

    private func interruptsWork(_ action: SetupAction) -> Bool {
        switch action.kind {
        case .switchAccount, .chatGptSignIn, .chatGptSignOut, .key, .run(step: "restart-proxy"): true
        default: false
        }
    }

    private var keyEntry: some View {
        HStack {
            SecureField("Paste the API key", text: $key)
                .textFieldStyle(.roundedBorder)
                .onSubmit(saveKey)
            Button("Save", action: saveKey).disabled(key.trimmingCharacters(in: .whitespaces).isEmpty)
            Button("Cancel") {
                enteringKey = false
                key = ""
            }
        }
    }

    private func saveKey() {
        guard case .key(let target) = check.action?.kind else { return }
        let value = key
        key = ""
        enteringKey = false
        Task { await model.saveKey(value, target: target, check: check.id) }
    }

    private func perform(_ action: SetupAction) {
        switch action.kind {
        case .run(let step): model.runSetupStep(step, check: check.id)
        case .openURL(let url): NSWorkspace.shared.open(url)
        case .signIn:
            code = ""
            model.signIn()
        case .chatGptSignIn(let account, let newAccount, let enablePlan):
            model.signInWithChatGPT(account: account, newAccount: newAccount, enablePlan: enablePlan)
        case .chatGptSignOut: model.signOutOfChatGPT()
        case .switchAccount: confirmingSwitch = true
        case .key: enteringKey = true
        }
    }
}

/// A running step: its stage, a progress bar, and for sign-in the page link and a code field.
private struct ActivityView: View {
    @Environment(AppModel.self) private var model
    let activity: SetupActivity
    @Binding var code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(activity.message).font(.callout)
            if let progress = activity.progress {
                ProgressView(value: progress)
                if let detail = activity.detail {
                    Text(detail).font(.caption).foregroundStyle(.secondary).monospacedDigit()
                }
            } else if !activity.needsCode {
                ProgressView().progressViewStyle(.linear)
            }
            if let url = activity.signInURL {
                Button("Open the approval page again") { NSWorkspace.shared.open(url) }
                    .buttonStyle(.link)
                    .font(.callout)
            }
            if activity.needsCode {
                HStack {
                    TextField("Code from the page", text: $code)
                        .textFieldStyle(.roundedBorder)
                        .onSubmit { model.sendSignInCode(code) }
                    Button("Continue") { model.sendSignInCode(code) }
                        .disabled(code.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
    }
}

private struct MicrophoneRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: model.micPermission == .authorized ? .ok : .action, running: false)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Text(title).font(.body.weight(.medium))
                Text(detail).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            switch model.micPermission {
            case .notDetermined:
                Button("Allow") { model.requestMicAccess() }.buttonStyle(.borderedProminent)
            case .denied, .restricted:
                Button("Open Settings") { openPrivacySettings("Privacy_Microphone") }
            default:
                EmptyView()
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private var title: String {
        model.micPermission == .authorized ? "Microphone allowed" : "Allow the microphone"
    }

    private var detail: String {
        switch model.micPermission {
        case .authorized: "Your side of each interview is recorded from it."
        case .notDetermined: "Needed to record your side of the interview."
        default: "Turn on Janus under Privacy & Security › Microphone, then come back."
        }
    }
}

/// Sharing your interviews' questions with the shared registry, for everyone's practice interviews.
private struct SharingRow: View {
    @Environment(AppModel.self) private var model
    @State private var withdrawn: String?

    var body: some View {
        let privacy = model.setup?.privacy
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: .optional, running: false).frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Toggle("Share my interview questions", isOn: Binding(
                    get: { privacy?.shareQuestions ?? false },
                    set: { model.setPrivacy("share-questions", $0) }
                ))
                .toggleStyle(.switch).font(.body.weight(.medium))
                Text("After each review, its interviewer questions are rewritten to name no person, company or product, checked again, and sent to Janus's question registry. Once approved, they're visible to other Janus users for practice interviews. Never your answers, transcripts, audio or video.")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                if privacy?.shareQuestions == true {
                    Toggle("Include the company (for review only, never published)", isOn: Binding(
                        get: { privacy?.shareCompany ?? false },
                        set: { model.setPrivacy("share-company", $0) }
                    ))
                    .toggleStyle(.checkbox).font(.callout)
                }
                HStack {
                    Button("Withdraw what I've shared") {
                        Task { withdrawn = await model.withdrawSharedQuestions() }
                    }
                    .buttonStyle(.link)
                    if let withdrawn { Text(withdrawn).font(.caption).foregroundStyle(.secondary) }
                }
                if let error = model.setupErrors["privacy"] {
                    Text(error).font(.caption).foregroundStyle(.red)
                }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }
}

/// Anonymous diagnostics: counts and outcomes, never content.
private struct DiagnosticsRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: .optional, running: false).frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Toggle("Send anonymous diagnostics", isOn: Binding(
                    get: { model.setup?.privacy?.diagnostics ?? false },
                    set: { model.setPrivacy("diagnostics", $0) }
                ))
                .toggleStyle(.switch).font(.body.weight(.medium))
                Text("Whether recording, reading the video, reviews and practice worked: counts, outcomes and warning codes, with a random id for this Mac. Never transcripts, questions, names, file paths, audio, video or faces.")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }
}

/// Screen Recording, for the call's video. Optional: without it, interviews record audio only.
private struct ScreenRecordingRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: model.screenPermission ? .ok : model.recordVideo ? .action : .optional, running: false)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Text(model.screenPermission ? "Screen Recording allowed" : "Allow Screen Recording for video")
                    .font(.body.weight(.medium))
                Text(detail).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            if !model.screenPermission {
                Button("Allow") { model.requestScreenAccess() }
            }
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private var detail: String {
        model.screenPermission
            ? "Janus records only the call's window, and reads the faces in it on this Mac."
            : "Optional: records the call's window so you can see how people reacted. After allowing it, reopen Janus."
    }
}

/// The self-test: proves both tracks really record, before an interview depends on it.
private struct RecordingTestRow: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            SetupStatusIcon(status: status, running: model.selfTest == .running)
                .frame(width: 20)
            VStack(alignment: .leading, spacing: 6) {
                Text("Test recording").font(.body.weight(.medium))
                ForEach(lines, id: \.self) { line in
                    Text(line).font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                }
                if case .finished(let result) = model.selfTest, !result.systemOK {
                    Button("Open Privacy & Security") { openPrivacySettings("Privacy_AudioCapture") }
                        .buttonStyle(.link)
                        .font(.callout)
                }
            }
            Spacer(minLength: 8)
            Button(model.selfTest == .notRun ? "Test (5 s)" : "Test again") { model.runSelfTest() }
                .disabled(model.selfTest == .running || model.phase.isRecording)
        }
        .padding(.horizontal, 18)
        .padding(.vertical, 12)
    }

    private var status: SetupCheck.Status {
        guard case .finished(let result) = model.selfTest else { return .optional }
        return result.micOK && result.systemOK ? .ok : .action
    }

    private var lines: [String] {
        switch model.selfTest {
        case .notRun:
            return ["Records 5 seconds and plays a short sound, to check both your mic and the call audio are captured. "
                + "Try it with the headphones you use for interviews."]
        case .running:
            return ["Recording… say something."]
        case .finished(let result):
            var lines = [
                result.micOK ? "Your mic: recorded." : "Your mic: nothing was recorded. Check the input device in System Settings › Sound.",
                result.systemOK
                    ? "Call audio: recorded."
                    : "Call audio: the test sound wasn't captured. Allow Janus under Privacy & Security › "
                        + "Screen & System Audio Recording (System Audio Recording Only), and check your volume isn't muted.",
            ]
            if !(result.micOK && result.systemOK) { lines += result.issues.prefix(2) }
            return lines
        }
    }
}

struct SetupStatusIcon: View {
    let status: SetupCheck.Status
    let running: Bool

    var body: some View {
        if running {
            ProgressView().controlSize(.small)
        } else {
            switch status {
            case .ok: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
            case .action: Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.orange)
            case .blocked: Image(systemName: "clock").foregroundStyle(.secondary)
            case .optional: Image(systemName: "circle.dashed").foregroundStyle(.secondary)
            }
        }
    }
}

/// System Settings › Privacy & Security, at the given pane when macOS recognises it.
func openPrivacySettings(_ anchor: String) {
    let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)")
        ?? URL(fileURLWithPath: "/System/Applications/System Settings.app")
    NSWorkspace.shared.open(url)
}
