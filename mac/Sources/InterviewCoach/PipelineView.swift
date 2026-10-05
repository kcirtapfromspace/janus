import InterviewCoachKit
import SwiftUI

/// Review is the main destination; the preceding evidence and next-round preparation stay nearby.
struct PipelineView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 18) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 7) {
                        Text([detail.session.company, detail.session.stage].compactMap { $0 }.joined(separator: " · "))
                            .font(.system(size: 10, weight: .medium)).foregroundStyle(CoachTheme.muted)
                        Text(detail.session.title)
                            .font(CoachTheme.editorial(27)).tracking(-0.6)
                            .foregroundStyle(CoachTheme.ink).lineLimit(2)
                        Text("\(String(detail.session.createdAt.prefix(10))) · \(formatDuration(detail.session.durationS))")
                            .font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
                    }
                    Spacer(minLength: 12)
                    if let session = model.selectedSession { VerdictBadge(session: session) }
                }
                FlowStrip(detail: detail)
                if let stale = detail.firstOutOfDate {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                        Text("\(stale.label) and later steps need an update.").font(.callout)
                        Spacer()
                        Button("Update steps") { model.updateLaterSteps() }
                            .disabled(model.phase.isBusy || detail.isBusy)
                    }
                }
            }
            .padding(22)
            .background(CoachTheme.canvas)
            Rectangle().fill(CoachTheme.line).frame(height: 1)
            Group {
                switch model.selectedStage {
                case .recording: RecordingStageView(detail: detail)
                case .transcript: TranscriptStageView(detail: detail)
                case .report: ReportStageView(detail: detail)
                case .next: NextStepsStageView(detail: detail)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
            .background(CoachTheme.surface)
        }
    }
}

struct FlowStrip: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        HStack(spacing: 5) {
            ForEach(detail.stages) { stage in
                Button { model.selectedStage = stage.step } label: {
                    StageCard(stage: stage, isSelected: model.selectedStage == stage.step)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(model.selectedStage == stage.step ? .isSelected : [])
            }
        }
        .overlay(alignment: .bottom) { CoachRule() }
    }
}

struct StageCard: View {
    let stage: StageState
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                Text(stage.step.shortTitle).font(.system(size: 12, weight: isSelected ? .semibold : .regular)).lineLimit(1)
                Spacer(minLength: 0)
                if stage.status == .running {
                    ProgressView().controlSize(.mini)
                } else if stage.status == .failed || stage.status == .outOfDate {
                    StatusIcon(status: stage.status).font(.system(size: 10))
                }
            }
            if stage.status != .done {
                Text(statusLine).font(.system(size: 10)).foregroundStyle(CoachTheme.muted).lineLimit(1)
            }
        }
        .foregroundStyle(isSelected ? CoachTheme.accent : CoachTheme.muted)
        .padding(.horizontal, 8).padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .frame(minHeight: 44, alignment: .top)
        .overlay(alignment: .bottom) {
            if isSelected { Rectangle().fill(CoachTheme.accent).frame(height: 2) }
        }
        .contentShape(Rectangle())
        .help("\(stage.label): \(stage.message ?? stage.summary ?? statusLine)")
    }

    private var statusLine: String {
        switch stage.status {
        case .running: stage.progress.map { "\(Int($0))% · processing" } ?? "Processing…"
        case .failed: "Needs attention"
        case .outOfDate: "Update available"
        case .notRun: "Not ready yet"
        case .done: "Ready"
        }
    }
}

struct StatusIcon: View {
    let status: StageStatus
    var body: some View {
        switch status {
        case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(CoachTheme.accent)
        case .running: ProgressView().controlSize(.mini)
        case .outOfDate: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .failed: Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        case .notRun: Image(systemName: "circle.dashed").foregroundStyle(.secondary)
        }
    }
}

func relativeTime(_ iso: String?) -> String? {
    guard let iso, let date = ISO8601DateFormatter().date(from: iso) else { return nil }
    let formatter = RelativeDateTimeFormatter()
    formatter.unitsStyle = .short
    return formatter.localizedString(for: date, relativeTo: Date())
}
