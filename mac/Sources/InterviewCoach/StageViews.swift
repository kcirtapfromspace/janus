import InterviewCoachKit
import SwiftUI

/// Models offered for re-running the report and next steps (the configured default is first).
private let modelChoices = ["anthropic/claude-opus-5-5", "anthropic/claude-sonnet-5-5", "openai/gpt-5.6"]

/// Title row of a stage pane: what it is, when and how it last ran, any problem, and its actions.
struct StageHeader<Actions: View>: View {
    let stage: StageState?
    let title: String
    @ViewBuilder let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .center) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.title3.weight(.semibold))
                    if !meta.isEmpty {
                        Text(meta).font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                actions
            }
            if let stage, stage.status == .failed, let error = stage.error {
                Label(error, systemImage: "xmark.octagon.fill")
                    .font(.callout)
                    .foregroundStyle(.red)
                    .textSelection(.enabled)
            }
            if let stage, stage.hasResult, stage.status != .running {
                ForEach(stage.warnings, id: \.self) { ProblemRow(problem: $0) }
            }
            if let stage, stage.status == .outOfDate {
                Label("Built from an earlier version of the previous stage. Re-run to update it.",
                      systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.orange)
            }
        }
        .padding(16)
    }

    private var meta: String {
        guard let stage else { return "" }
        var parts: [String] = []
        if let model = stage.model { parts.append(model) }
        if let when = relativeTime(stage.lastRunAt) { parts.append("ran \(when)") }
        if let seconds = stage.durationS, stage.status != .running { parts.append("took \(formatDuration(seconds))") }
        return parts.joined(separator: " · ")
    }
}

/// A stage with nothing to show yet.
struct EmptyStage: View {
    @Environment(AppModel.self) private var model
    let stage: StageState?
    let step: StageStep

    var body: some View {
        ContentUnavailableView {
            Label(stage?.status == .running ? "Running…" : "Not run yet", systemImage: "circle.dashed")
        } description: {
            Text(stage?.message ?? stage?.rerunBlocked ?? "This stage hasn't produced anything yet.")
        } actions: {
            if stage?.status != .running {
                Button("Run \(stage?.label.lowercased() ?? "stage")") { model.rerun(step, thenLater: true) }
                    .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
            }
        }
    }
}

/// Play/pause and a scrubber over the listening copy.
struct PlayerBar: View {
    let player: AudioPlayer

    var body: some View {
        HStack(spacing: 10) {
            Button { player.togglePlay() } label: {
                Image(systemName: player.isPlaying ? "pause.fill" : "play.fill").frame(width: 16)
            }
            .keyboardShortcut(.space, modifiers: [])
            Slider(value: Binding(get: { player.currentTime }, set: { player.seek(to: $0) }),
                   in: 0...max(player.duration, 1))
            Text("\(formatDuration(player.currentTime)) / \(formatDuration(player.duration))")
                .font(.caption.monospacedDigit())
                .foregroundStyle(.secondary)
        }
        .disabled(!player.isLoaded)
    }
}

/// A quoted line; its timestamp plays the audio from that moment.
struct QuoteView: View {
    @Environment(AppModel.self) private var model
    let evidence: Evidence

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Button(evidence.timestamp) {
                if let t = seconds(fromTimestamp: evidence.timestamp) { model.player.play(from: t) }
            }
            .buttonStyle(.link)
            .font(.caption.monospacedDigit())
            .help("Play from here")
            Text("“\(evidence.quote)”").italic().textSelection(.enabled)
        }
        .padding(.leading, 10)
        .overlay(alignment: .leading) { Rectangle().fill(.quaternary).frame(width: 3) }
    }
}

// MARK: - 1. Recording

struct RecordingStageView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        let stage = detail.stage(.recording)
        VStack(alignment: .leading, spacing: 0) {
            StageHeader(stage: stage, title: "Recording") {
                Button("Show in Finder") {
                    NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: detail.session.dir)])
                }
                Button("Re-process audio") { model.rerun(.recording) }
                    .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
                    .help(stage?.rerunBlocked ?? "Rebuild the audio files and the listening copy from the original recording")
            }
            if stage?.hasResult == true {
                VStack(alignment: .leading, spacing: 16) {
                    if detail.audio.listenPath != nil {
                        PlayerBar(player: model.player)
                    } else {
                        Text("There's no listening copy yet — re-process the audio to make one.")
                            .foregroundStyle(.secondary)
                    }
                    Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 6) {
                        GridRow {
                            Text("Length").foregroundStyle(.secondary)
                            Text(formatDuration(detail.session.durationS))
                        }
                        GridRow {
                            Text("Source").foregroundStyle(.secondary)
                            Text(detail.session.source == "recording" ? "Recorded in the app" : "Imported")
                        }
                        ForEach(detail.audio.tracks) { track in
                            GridRow {
                                Text(track.label).foregroundStyle(.secondary)
                                Text(track.path).font(.caption.monospaced()).textSelection(.enabled).lineLimit(1)
                                    .truncationMode(.middle)
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
                Spacer()
            } else {
                EmptyStage(stage: stage, step: .recording)
            }
        }
    }
}

// MARK: - 2. Transcript

struct TranscriptStageView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        let stage = detail.stage(.transcript)
        VStack(spacing: 0) {
            StageHeader(stage: stage, title: "Transcript") {
                if detail.session.isSingleTrack {
                    Button("Swap speakers") { model.swapSpeakers() }
                        .disabled(stage?.hasResult != true || model.phase.isBusy)
                        .help("If You and Interviewer are the wrong way round")
                    Menu("Re-run") {
                        ForEach(2...5, id: \.self) { n in
                            Button("\(n) people on the call") { model.rerun(.transcript, options: ["--speakers", "\(n)"]) }
                        }
                    }
                    .fixedSize()
                    .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
                } else {
                    Button("Re-run transcription") { model.rerun(.transcript) }
                        .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
                        .help(stage?.rerunBlocked ?? "Transcribe the recording again")
                }
            }
            if detail.turns.isEmpty {
                EmptyStage(stage: stage, step: .transcript)
            } else {
                if detail.audio.listenPath != nil {
                    PlayerBar(player: model.player).padding(.horizontal, 16).padding(.bottom, 8)
                }
                List(detail.turns) { turn in
                    let playing = model.player.isPlaying && (turn.start...turn.end).contains(model.player.currentTime)
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Button(turn.timestamp) { model.player.play(from: turn.start) }
                            .buttonStyle(.link)
                            .font(.caption.monospacedDigit())
                            .help("Play from here")
                        Text(turn.speakerLabel)
                            .fontWeight(.semibold)
                            .foregroundStyle(turn.isYou ? Color.blue : Color.purple)
                            .frame(width: 92, alignment: .leading)
                        Text(turn.text).textSelection(.enabled)
                    }
                    .padding(.vertical, 3)
                    .listRowBackground(playing ? Color.accentColor.opacity(0.12) : Color.clear)
                }
            }
        }
    }
}

// MARK: - 3. After-action report

struct ReportStageView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail
    @State private var chosen: Int?

    var body: some View {
        let stage = detail.stage(.report)
        let shown = detail.reports.first { $0.analysisId == chosen } ?? detail.reports.first { $0.isCurrent }
            ?? detail.reports.first
        VStack(spacing: 0) {
            StageHeader(stage: stage, title: "After-action report") {
                if detail.reports.count > 1 {
                    Picker("Run", selection: Binding(get: { shown?.analysisId }, set: { chosen = $0 })) {
                        ForEach(detail.reports) { r in
                            Text("\(relativeTime(r.createdAt) ?? r.createdAt) · \(r.model) · \(r.verdictLabel)\(r.isCurrent ? " (current)" : "")")
                                .tag(Optional(r.analysisId))
                        }
                    }
                    .frame(maxWidth: 380)
                }
                RerunMenu(step: .report, stage: stage, defaultModel: model.setup?.model)
                Menu("Outcome") {
                    ForEach(outcomeChoices, id: \.value) { choice in
                        Button(choice.label) { model.setOutcome(detail.session.id, choice.value) }
                    }
                }
                .fixedSize()
                .disabled(model.phase.isBusy)
                Button("Open in Browser") {
                    if let path = shown?.htmlPath { NSWorkspace.shared.open(URL(fileURLWithPath: path)) }
                }
                .disabled(shown == nil)
            }
            if let shown {
                ReportView(path: shown.htmlPath)
            } else {
                EmptyStage(stage: stage, step: .report)
            }
        }
        .onChange(of: detail.session.id) { chosen = nil }
    }
}

/// "Re-run" with the default model, or pick another one.
struct RerunMenu: View {
    @Environment(AppModel.self) private var model
    let step: StageStep
    let stage: StageState?
    let defaultModel: String?

    var body: some View {
        Menu("Re-run") {
            Button("Re-run with \(stage?.model ?? defaultModel ?? "the default model")") { model.rerun(step) }
            Divider()
            ForEach(modelChoices, id: \.self) { choice in
                Button(choice) { model.rerun(step, options: ["--model", choice]) }
            }
        }
        .fixedSize()
        .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
        .help(stage?.rerunBlocked ?? "Run this stage again; earlier runs are kept")
    }
}

// MARK: - 4. What to do next

struct NextStepsStageView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        let stage = detail.stage(.next)
        VStack(spacing: 0) {
            StageHeader(stage: stage, title: "What to do next") {
                RerunMenu(step: .next, stage: stage, defaultModel: detail.stage(.report)?.model)
            }
            if let next = detail.nextSteps {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        Text(next.plan.headline).font(.title3)
                        Text("Next-round prep").font(.headline)
                        ForEach(next.plan.nextRoundPrep) { PrepCard(item: $0) }
                        Text("Practice plan").font(.headline)
                        ForEach(Array(next.plan.practicePlan.enumerated()), id: \.offset) { index, item in
                            PracticeRow(item: item, key: "ic.practice.\(next.id).\(index)")
                        }
                        if !next.unverifiedQuotes.isEmpty {
                            Text("\(next.unverifiedQuotes.count) quoted line(s) aren't word-for-word in the transcript.")
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                    }
                    .padding(.horizontal, 16)
                    .padding(.bottom, 16)
                    .frame(maxWidth: 820, alignment: .leading)
                }
            } else {
                EmptyStage(stage: stage, step: .next)
            }
        }
    }
}

struct PrepCard: View {
    let item: PrepItem

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(item.topic).font(.body.weight(.semibold))
            Text(item.why).foregroundStyle(.secondary)
            QuoteView(evidence: item.evidence)
            Text("**Prepare:** \(item.howToPrepare)")
            if !item.likelyQuestions.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(item.likelyQuestions, id: \.self) { question in
                        Label(question, systemImage: "questionmark.bubble").font(.callout)
                    }
                }
            }
        }
        .textSelection(.enabled)
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 10).strokeBorder(Color.secondary.opacity(0.2)))
    }
}

/// A drill with a checkbox that's remembered on this Mac.
struct PracticeRow: View {
    let item: PracticeItem
    let key: String
    @State private var done = false

    var body: some View {
        Toggle(isOn: $done) {
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(item.skill).fontWeight(.semibold)
                    Text("\(item.minutes) min").font(.caption).foregroundStyle(.secondary)
                    Text(item.priority).font(.caption2.weight(.semibold))
                        .padding(.horizontal, 6).padding(.vertical, 1)
                        .background(priorityColor.opacity(0.15), in: Capsule())
                        .foregroundStyle(priorityColor)
                }
                Text(item.drill).strikethrough(done)
                Text("For: \(item.fromCoaching)").font(.caption).foregroundStyle(.secondary)
            }
        }
        .toggleStyle(.checkbox)
        .onAppear { done = UserDefaults.standard.bool(forKey: key) }
        .onChange(of: done) { UserDefaults.standard.set(done, forKey: key) }
    }

    private var priorityColor: Color {
        switch item.priority {
        case "high": .red
        case "medium": .orange
        default: .secondary
        }
    }
}
