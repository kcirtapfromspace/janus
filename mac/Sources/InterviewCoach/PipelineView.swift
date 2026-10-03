import InterviewCoachKit
import SwiftUI

/// One interview as its four stages: a flow strip across the top showing where each stands,
/// and the selected stage's content and actions below.
struct PipelineView: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 10) {
                FlowStrip(detail: detail)
                if let stale = detail.firstOutOfDate {
                    HStack(spacing: 8) {
                        Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
                        Text("\(stale.label) and the stages after it were built from an earlier version.")
                            .font(.callout)
                        Spacer()
                        Button("Update later steps") { model.updateLaterSteps() }
                            .disabled(model.phase.isBusy || detail.isBusy)
                    }
                }
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 12)
            Divider()
            Group {
                switch model.selectedStage {
                case .recording: RecordingStageView(detail: detail)
                case .transcript: TranscriptStageView(detail: detail)
                case .report: ReportStageView(detail: detail)
                case .next: NextStepsStageView(detail: detail)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        }
    }
}

struct FlowStrip: View {
    @Environment(AppModel.self) private var model
    let detail: SessionDetail

    var body: some View {
        HStack(spacing: 6) {
            ForEach(detail.stages) { stage in
                Button { model.selectedStage = stage.step } label: {
                    StageCard(stage: stage, isSelected: model.selectedStage == stage.step)
                }
                .buttonStyle(.plain)
                if stage.step != .next {
                    Image(systemName: "chevron.right").foregroundStyle(.tertiary)
                }
            }
        }
    }
}

struct StageCard: View {
    let stage: StageState
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(spacing: 6) {
                StatusIcon(status: stage.status)
                Text(stage.label).font(.callout.weight(.semibold)).lineLimit(1)
            }
            Text(statusLine)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(2, reservesSpace: true)
            if stage.status == .running {
                if let progress = stage.progress {
                    ProgressView(value: progress, total: 100).controlSize(.small)
                } else {
                    ProgressView().progressViewStyle(.linear).controlSize(.small)
                }
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 10)
            .fill(isSelected ? Color.accentColor.opacity(0.12) : Color(nsColor: .controlBackgroundColor)))
        .overlay(RoundedRectangle(cornerRadius: 10)
            .strokeBorder(isSelected ? Color.accentColor : Color.secondary.opacity(0.25)))
        .contentShape(RoundedRectangle(cornerRadius: 10))
    }

    private var statusLine: String {
        switch stage.status {
        case .running: return stage.message ?? "Running…"
        case .failed: return "Failed" + (stage.summary.map { _ in " · showing the last good result" } ?? "")
        case .outOfDate: return "Out of date · " + (stage.summary ?? "")
        case .notRun: return "Not run yet"
        case .done:
            return [stage.summary, relativeTime(stage.lastRunAt)].compactMap { $0 }.joined(separator: " · ")
        }
    }
}

struct StatusIcon: View {
    let status: StageStatus

    var body: some View {
        switch status {
        case .done: Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
        case .running: ProgressView().controlSize(.mini)
        case .outOfDate: Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        case .failed: Image(systemName: "xmark.octagon.fill").foregroundStyle(.red)
        case .notRun: Image(systemName: "circle.dashed").foregroundStyle(.secondary)
        }
    }
}

/// "5 min ago" from ic's ISO 8601 timestamps.
func relativeTime(_ iso: String?) -> String? {
    guard let iso, let date = ISO8601DateFormatter().date(from: iso) else { return nil }
    let formatter = RelativeDateTimeFormatter()
    formatter.unitsStyle = .short
    return formatter.localizedString(for: date, relativeTo: Date())
}
