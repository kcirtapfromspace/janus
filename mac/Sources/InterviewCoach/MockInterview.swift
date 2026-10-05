import AVFoundation
import ICRecorderCore
import InterviewCoachKit
import Observation
import SwiftUI

/// One mock interview, from planning to its review. `ic mock begin` plans the questions and creates
/// the session; MockRecorder records it as a two-track interview; `ic mock run` is the interviewer
/// (it hears each answer and says what's next); `ic recording finish` reviews it like any other.
@MainActor @Observable
final class MockInterview {
    enum Phase: Equatable {
        case setup
        case preparing(String)
        case speaking
        case answering
        case thinking(String)
        case finishing
        case failed(String)
    }

    var phase: Phase = .setup
    /// What the interviewer said last.
    var caption = ""
    var questions: [String] = []
    /// Interviewer turns so far.
    var turns = 0
    var level: Float = 0
    var company = ""
    var role = ""
    var round = ""
    var count = 5

    @ObservationIgnored private var recorder: MockRecorder?
    @ObservationIgnored private var stream: ICStream?
    @ObservationIgnored private var sessionID: Int?
    @ObservationIgnored private var meter: Timer?

    var isRunning: Bool {
        switch phase {
        case .speaking, .answering, .thinking, .preparing: true
        default: false
        }
    }

    private struct Begun: Decodable {
        struct Planned: Decodable { let text: String }
        let id: Int
        let dir: String
        let plan: [Planned]
    }

    func start(model: AppModel) async {
        guard let ic = model.ic, !model.phase.isBusy, !isRunning else { return }
        phase = .preparing("Checking your microphone…")
        guard await Self.micAllowed() else {
            phase = .failed("Janus needs the microphone to hear your answers. Allow it in System Settings › Privacy & Security › Microphone.")
            return
        }
        phase = .preparing("Choosing questions from your interviews…")
        model.phase = .working("Practice interview")
        do {
            var args = ["mock", "begin", "--count", "\(count)"]
            for (flag, value) in [("--company", company), ("--role", role), ("--round", round)] where !value.trimmingCharacters(in: .whitespaces).isEmpty {
                args += [flag, value.trimmingCharacters(in: .whitespaces)]
            }
            let begun = try await ic.decode(Begun.self, args)
            sessionID = begun.id
            questions = begun.plan.map(\.text)
            let dir = URL(fileURLWithPath: begun.dir, isDirectory: true)
            let recorder = MockRecorder(sessionDir: dir, log: Logger(fileURL: dir.appendingPathComponent("recorder.log")))
            try recorder.start()
            self.recorder = recorder
            phase = .preparing("Getting the interviewer ready…")
            let stream = try ic.stream(["mock", "run", "\(begun.id)", "--events"]) { [weak self] event in
                self?.handle(event, model: model)
            }
            self.stream = stream
            meter = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, let recorder = self.recorder else { return }
                    self.level = self.phase == .answering ? recorder.takeLevel() : 0
                }
            }
            Task {
                do {
                    try await stream.wait()
                } catch {
                    if self.isRunning { self.fail(error.localizedDescription, model: model) }
                }
            }
        } catch {
            fail(error.localizedDescription, model: model)
        }
    }

    private func handle(_ event: SetupEvent, model: AppModel) {
        switch event {
        case .stage(let message):
            if case .thinking = phase { phase = .thinking(message + "…") }
            if case .preparing = phase { phase = .preparing(message + "…") }
        case .say(let text, let done):
            caption = text
            turns += 1
            phase = .speaking
            recorder?.speak(text) { [weak self] in
                guard let self, self.phase == .speaking else { return }
                if done {
                    self.finish(model: model)
                } else {
                    self.recorder?.beginAnswer()
                    self.phase = .answering
                }
            }
        case .error(let message):
            fail(message, model: model)
        default:
            break
        }
    }

    /// You've finished your answer: the interviewer hears it and says what's next.
    func doneAnswering() {
        guard phase == .answering, let recorder, let stream else { return }
        do {
            let url = try recorder.endAnswer()
            let line = try JSONSerialization.data(withJSONObject: ["answer": url.path])
            stream.send(String(decoding: line, as: UTF8.self))
            phase = .thinking("Listening back…")
        } catch {
            phase = .answering
        }
    }

    /// End early; what was recorded is still reviewed.
    func end(model: AppModel) {
        guard isRunning else { return }
        stream?.send(#"{"stop": true}"#)
        finish(model: model)
    }

    private func finish(model: AppModel) {
        meter?.invalidate()
        phase = .finishing
        let saved = recorder?.stop(reason: "mock interview ended") ?? false
        recorder = nil
        stream = nil
        model.phase = .idle
        if saved, let id = sessionID {
            model.finishMock(id)
        }
    }

    private func fail(_ message: String, model: AppModel) {
        meter?.invalidate()
        stream?.cancel()
        stream = nil
        recorder?.stop(reason: "error")
        recorder = nil
        if model.phase == .working("Practice interview") { model.phase = .idle }
        phase = .failed(message)
    }

    private static func micAllowed() async -> Bool {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return true
        case .notDetermined: return await AVCaptureDevice.requestAccess(for: .audio)
        default: return false
        }
    }
}

/// The practice window: choose what to practise, then talk with the interviewer.
struct MockInterviewWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismiss) private var dismiss
    @State private var mock = MockInterview()

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack(spacing: 14) {
                BrandMark(size: 40)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Practice interview").font(CoachTheme.editorial(23)).foregroundStyle(CoachTheme.ink)
                    Text("Real questions from your interviews, asked out loud, then reviewed like the real thing.")
                        .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                }
            }
            switch mock.phase {
            case .setup, .failed:
                setup
            case .finishing:
                ProgressView("Saving your practice interview, then reviewing it…").frame(maxWidth: .infinity)
            default:
                conversation
            }
        }
        .padding(26)
        .frame(width: 520)
        .background(CoachTheme.canvas)
        .onChange(of: mock.phase) { _, phase in
            if phase == .finishing {
                openWindow(id: "main")
                dismiss()
            }
        }
    }

    private var setup: some View {
        @Bindable var mock = mock
        return VStack(alignment: .leading, spacing: 14) {
            Grid(alignment: .leading, horizontalSpacing: 12, verticalSpacing: 10) {
                GridRow {
                    Text("Company").foregroundStyle(.secondary)
                    TextField("Optional", text: $mock.company)
                }
                GridRow {
                    Text("Role").foregroundStyle(.secondary)
                    TextField("Optional, e.g. Product manager", text: $mock.role)
                }
                GridRow {
                    Text("Round").foregroundStyle(.secondary)
                    TextField("Optional, e.g. Hiring manager", text: $mock.round)
                }
                GridRow {
                    Text("Questions").foregroundStyle(.secondary)
                    Stepper("\(mock.count)", value: $mock.count, in: 3...8)
                }
            }
            .textFieldStyle(.roundedBorder)
            Text("Questions you've been asked before come first: the company and role you name, then the answers you found hardest. Classic questions fill in. Headphones keep the interviewer's voice out of your recording.")
                .font(.system(size: 11)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
            Text("Your answers are transcribed on this Mac. The transcript goes to your coaching model for the interviewer's next question and for the review, as with a real interview.")
                .font(.system(size: 10)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
            if case .failed(let message) = mock.phase {
                Text(message).font(.callout).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
            }
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { dismiss() }.keyboardShortcut(.cancelAction)
                Button {
                    Task { await mock.start(model: model) }
                } label: { Label("Start", systemImage: "person.wave.2") }
                .buttonStyle(CoachPrimaryButtonStyle())
                .keyboardShortcut(.defaultAction)
                .disabled(model.phase.isBusy)
            }
        }
    }

    private var conversation: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(mock.caption.isEmpty ? " " : mock.caption)
                .font(.system(size: 15))
                .frame(maxWidth: .infinity, minHeight: 90, alignment: .topLeading)
                .padding(14)
                .background(CoachTheme.canvas.opacity(0.6))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(.quaternary))
            HStack(spacing: 10) {
                switch mock.phase {
                case .preparing(let message), .thinking(let message):
                    ProgressView().controlSize(.small)
                    Text(message).foregroundStyle(.secondary)
                case .speaking:
                    Image(systemName: "speaker.wave.2.fill").foregroundStyle(CoachTheme.accent)
                    Text("The interviewer is speaking").foregroundStyle(.secondary)
                case .answering:
                    Image(systemName: "mic.fill").foregroundStyle(.red)
                    Text("Your turn: answer out loud").foregroundStyle(.secondary)
                    ProgressView(value: Double(min(mock.level * 4, 1))).frame(width: 90)
                default:
                    EmptyView()
                }
                Spacer()
                if !mock.questions.isEmpty {
                    Text("\(mock.questions.count) questions planned").font(.caption).foregroundStyle(.secondary)
                }
            }
            HStack {
                Button("End interview", role: .destructive) { mock.end(model: model) }
                Spacer()
                Button {
                    mock.doneAnswering()
                } label: { Label("Done answering", systemImage: "checkmark.circle") }
                .buttonStyle(CoachPrimaryButtonStyle())
                .keyboardShortcut(.space, modifiers: [])
                .disabled(mock.phase != .answering)
            }
            Text("Press Space when you've finished an answer. Ending early still reviews what you said.")
                .font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
        }
    }
}
