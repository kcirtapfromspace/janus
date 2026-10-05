import InterviewCoachKit
import SwiftUI

/// Models offered for re-running the report and next steps (the configured default is first).

/// Title row of a stage pane: what it is, when and how it last ran, any problem, and its actions.
struct StageHeader<Actions: View>: View {
    let stage: StageState?
    let title: String
    @ViewBuilder let actions: Actions

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            // Actions sit beside the title when they fit, and move under it when they don't;
            // a button's text is never cut off.
            ViewThatFits(in: .horizontal) {
                HStack(alignment: .center, spacing: 12) {
                    titleBlock
                    Spacer(minLength: 12)
                    actionRow
                }
                VStack(alignment: .leading, spacing: 10) {
                    titleBlock
                    actionRow
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
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

    private var titleBlock: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.system(size: 18, weight: .medium)).fixedSize()
            if !meta.isEmpty {
                Text(meta).font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var actionRow: some View {
        HStack(spacing: 8) { actions }.fixedSize()
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

/// Play/pause and a scrubber over the listening copy. With `videoToggle`, a button shows or hides
/// the call's video above the page.
struct PlayerBar: View {
    let player: AudioPlayer
    var videoToggle = false
    @AppStorage("showCallVideo") private var showVideo = true

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
            if videoToggle && player.hasVideo {
                Button { showVideo.toggle() } label: {
                    Image(systemName: showVideo ? "video.fill" : "video.slash").frame(width: 18)
                }
                .buttonStyle(.borderless)
                .help(showVideo ? "Hide the call's video" : "Show the call's video")
            }
        }
        .disabled(!player.isLoaded)
    }
}

/// The call's video at the current moment, when it was recorded. `compact` is the strip above a
/// transcript or report (hidden with the player bar's button); otherwise it's the Recording tab's.
struct CallVideo: View {
    let player: AudioPlayer
    var compact = false
    @AppStorage("showCallVideo") private var showVideo = true

    var body: some View {
        if player.hasVideo, let avPlayer = player.player, !compact || showVideo {
            CallVideoView(player: avPlayer)
                .aspectRatio(16 / 9, contentMode: .fit)
                .frame(maxHeight: compact ? 200 : 420)
                .background(Color.black)
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .frame(maxWidth: .infinity)
        }
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
            .foregroundStyle(CoachTheme.accent)
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
                        CallVideo(player: model.player)
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
                        if let video = detail.audio.videoPath {
                            GridRow {
                                Text("Call video").foregroundStyle(.secondary)
                                Text(video).font(.caption.monospaced()).textSelection(.enabled).lineLimit(1)
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
                    CallVideo(player: model.player, compact: true).padding(.horizontal, 16).padding(.bottom, 8)
                    PlayerBar(player: model.player, videoToggle: true).padding(.horizontal, 16).padding(.bottom, 8)
                }
                List(detail.turns) { turn in
                    let playing = model.player.isPlaying && (turn.start...turn.end).contains(model.player.currentTime)
                    HStack(alignment: .firstTextBaseline, spacing: 10) {
                        Button(turn.timestamp) { model.player.play(from: turn.start) }
                            .buttonStyle(.link)
                            .foregroundStyle(CoachTheme.accent)
                            .font(.caption.monospacedDigit())
                            .help("Play from here")
                        Text(turn.speakerLabel)
                            .fontWeight(.semibold)
                            .foregroundStyle(turn.isYou ? CoachTheme.accent : CoachTheme.muted)
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
    /// The answer being corrected (its start), while the Correct video cues sheet is open.
    @State private var correcting: Double?

    var body: some View {
        let stage = detail.stage(.report)
        let shown = detail.reports.first { $0.analysisId == chosen } ?? detail.reports.first { $0.isCurrent }
            ?? detail.reports.first
        VStack(spacing: 0) {
            StageHeader(stage: stage, title: "Your review") {
                if detail.reports.count > 1 {
                    // Oldest first, numbered like the report's history (v1, v2, …).
                    let versions = Array(detail.reports.sorted { $0.analysisId < $1.analysisId }.enumerated())
                    Menu("Versions (\(detail.reports.count))") {
                        ForEach(versions, id: \.element.analysisId) { index, r in
                            Toggle(isOn: Binding(get: { shown?.analysisId == r.analysisId }, set: { _ in chosen = r.analysisId })) {
                                Text("v\(index + 1) · \(r.model.split(separator: "/").last.map(String.init) ?? r.model) · \(r.verdictLabel)\(r.isCurrent ? " (current)" : "")")
                            }
                        }
                    }
                    .help("Every version is kept. The report's Versions section shows how they relate.")
                }
                if let first = detail.correctableAnswers.first {
                    Button("Correct video cues…") { correcting = first.start }
                        .help("Say what the video really showed during an answer: it helps measure these cues")
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
                if detail.audio.listenPath != nil {
                    CallVideo(player: model.player, compact: true).padding(.horizontal, 16).padding(.bottom, 8)
                    PlayerBar(player: model.player, videoToggle: true).padding(.horizontal, 16).padding(.bottom, 8)
                }
                ReportView(path: shown.htmlPath, onSeek: { model.player.play(from: $0) }, onOpenReport: { chosen = $0 },
                           onFix: { start in correcting = start })
            } else {
                EmptyStage(stage: stage, step: .report)
            }
        }
        .onChange(of: detail.session.id) { chosen = nil }
        .sheet(isPresented: Binding(get: { correcting != nil }, set: { if !$0 { correcting = nil } })) {
            VideoCorrectionsSheet(answers: detail.correctableAnswers, selected: correcting ?? 0) { correcting = nil }
        }
    }
}

/// Correct video cues: every answer the video covered, with what it measured; pick one, watch it,
/// and say what you saw. Saved as a label for measuring accuracy (`ic eval correct`).
struct VideoCorrectionsSheet: View {
    @Environment(AppModel.self) private var model
    let answers: [SessionDetail.VideoAnswer]
    @State var selected: Double
    let done: () -> Void
    @State private var form: VideoCorrection?
    @State private var status: String?
    @State private var saving = false

    private var answer: SessionDetail.VideoAnswer? { answers.first { $0.start == selected } ?? answers.first }

    var body: some View {
        HSplitView {
            List(answers, selection: Binding(get: { answer?.id }, set: { if let id = $0 { selected = id } })) { a in
                VStack(alignment: .leading, spacing: 2) {
                    HStack {
                        Text(a.timestamp).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                        if a.corrected != nil {
                            Text("corrected").font(.caption2).foregroundStyle(CoachTheme.accent)
                        }
                    }
                    Text(a.question ?? "Your answer").lineLimit(2)
                    Text(a.notes.isEmpty ? "Nothing noted" : a.notes.joined(separator: ", "))
                        .font(.caption).foregroundStyle(.secondary).lineLimit(2)
                }
                .tag(a.id)
            }
            .frame(minWidth: 220, idealWidth: 260)
            ScrollView {
                VStack(alignment: .leading, spacing: 14) {
                    if let answer {
                        if model.player.hasVideo, let player = model.player.player {
                            CallVideoView(player: player)
                                .aspectRatio(16 / 9, contentMode: .fit)
                                .frame(maxHeight: 240)
                                .background(Color.black)
                                .clipShape(RoundedRectangle(cornerRadius: 6))
                        }
                        HStack {
                            Button("Play this answer") { model.player.play(from: answer.start) }
                            Text("\(answer.timestamp), \(formatDuration(answer.end - answer.start))")
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Text("What the review measured: \(answer.notes.isEmpty ? "nothing noted" : answer.notes.joined(separator: ", ")). Change what it got wrong.")
                            .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        if let binding = Binding($form) {
                            ForEach(VideoCorrection.checks) { check in
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(check.title).font(.callout.weight(.semibold))
                                    Text(check.hint).font(.caption).foregroundStyle(.secondary)
                                    Picker(check.title, selection: Binding(
                                        get: { binding.wrappedValue.pick(check.id) },
                                        set: { binding.wrappedValue.set(check.id, $0) }
                                    )) {
                                        ForEach(check.options, id: \.value) { option in Text(option.title).tag(option.value) }
                                        Text("Can't tell").tag(VideoCorrection.unsure)
                                    }
                                    .pickerStyle(.segmented).labelsHidden()
                                }
                            }
                        }
                        Text("Saved on this Mac with your labels, to measure how accurate these cues are. It doesn't change the review.")
                            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
                        if let status {
                            Text(status).font(.callout).foregroundStyle(status.hasPrefix("Saved") ? CoachTheme.accent : .red)
                        }
                    }
                }
                .padding(16)
            }
            .frame(minWidth: 420)
        }
        .frame(minWidth: 720, minHeight: 520)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Done") { done() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save correction") {
                    guard let answer, let form else { return }
                    saving = true
                    Task {
                        let error = await model.correctVideo(start: answer.start, correction: form)
                        status = error ?? "Saved your correction for \(answer.timestamp)."
                        saving = false
                    }
                }
                .disabled(form == nil || saving)
            }
        }
        .onAppear { loadForm() }
        .onChange(of: selected) { loadForm() }
    }

    private func loadForm() {
        status = nil
        guard let answer else { form = nil; return }
        form = answer.corrected.map(VideoCorrection.init(corrected:)) ?? VideoCorrection(measured: answer.cues)
    }
}

/// "Re-run": with the same model, the cheapest one available, or any model your accounts can use
/// (from `ic models`, with list prices).
struct RerunMenu: View {
    @Environment(AppModel.self) private var model
    let step: StageStep
    let stage: StageState?
    let defaultModel: String?

    var body: some View {
        Menu("Re-run") {
            if let last = stage?.model {
                Button("Same model (\(shortName(last)))") { model.rerun(step, options: ["--model", last]) }
            } else {
                Button("With \(defaultModel.map(shortName) ?? "the default model")") { model.rerun(step) }
            }
            if model.modelOffers.contains(where: \.cheapest) {
                Button(cheapestLabel) { model.rerun(step, options: ["--model", "cheapest"]) }
            }
            Divider()
            if model.modelOffers.isEmpty {
                Text("Loading your models…")
            } else {
                Section("Choose a model (list price)") {
                    ForEach(model.modelOffers) { offer in
                        Button(offer.priceLabel.map { "\(offer.name) — \($0)" } ?? offer.name) {
                            model.rerun(step, options: ["--model", offer.model])
                        }
                    }
                }
            }
        }
        .fixedSize()
        .disabled(!(stage?.canRerun ?? false) || model.phase.isBusy)
        .help(stage?.rerunBlocked ?? help)
        .task { if model.modelOffers.isEmpty { await model.loadModelOffers() } }
    }

    private var help: String {
        step == .report
            ? "Nothing changed? You get the same report back, with no new model call. Another model makes a new version; every version is kept."
            : "Run this stage again; earlier runs are kept"
    }

    private var cheapestLabel: String {
        guard let cheapest = model.modelOffers.first(where: \.cheapest) else { return "Cheapest available" }
        return "Cheapest available (\(cheapest.name)\(cheapest.priceLabel.map { ", \($0)" } ?? ""))"
    }
}

private func shortName(_ model: String) -> String {
    model.split(separator: "/", maxSplits: 1).last.map(String.init) ?? model
}

// MARK: - 4. What to do next

struct NextStepsStageView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        let stage = detail.stage(.next)
        VStack(spacing: 0) {
            StageHeader(stage: stage, title: "Prepare for the next round") {
                RerunMenu(step: .next, stage: stage, defaultModel: detail.stage(.report)?.model)
            }
            if let next = detail.nextSteps {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        Text(next.plan.headline).font(CoachTheme.editorial(17)).foregroundStyle(CoachTheme.ink)
                            .lineSpacing(5).frame(maxWidth: 640, alignment: .leading)
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
        .padding(.vertical, 16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(alignment: .bottom) { CoachRule() }
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
        case "high": CoachTheme.alert
        case "medium": CoachTheme.caution
        default: .secondary
        }
    }
}
