import InterviewCoachKit
import SwiftUI

/// "3:09" or "1:02:03".
func formatDuration(_ seconds: Double?) -> String {
    let s = Int(seconds ?? 0)
    return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s % 3600 / 60, s % 60)
                     : String(format: "%d:%02d", s / 60, s % 60)
}

/// Elapsed time since `start`, updating every second.
struct ElapsedTime: View {
    let since: Date

    var body: some View {
        TimelineView(.periodic(from: since, by: 1)) { context in
            Text(formatDuration(context.date.timeIntervalSince(since)))
                .monospacedDigit()
        }
    }
}

/// The analysis verdict, or the session's status if it hasn't been analysed.
struct VerdictBadge: View {
    let session: SessionSummary

    var label: String {
        if let verdict = session.verdictLabel { return verdict }
        switch session.status {
        case "recording": return "Recording"
        case "failed": return "Failed"
        case "transcribed": return "Not analysed"
        default: return session.status.capitalized
        }
    }

    private var color: Color {
        switch session.verdict ?? session.status {
        case "strong", "leaning_positive": .green
        case "mixed": .orange
        case "leaning_negative", "weak", "failed", "recording": .red
        default: .secondary
        }
    }

    var body: some View {
        Text(label)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .foregroundStyle(color)
            .background(color.opacity(0.15), in: Capsule())
    }
}

struct SessionRow: View {
    let session: SessionSummary
    /// Under its role in the sidebar: the round leads, since the company and role are above it.
    var inRole = false

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline) {
                Text(session.title)
                    .fontWeight(.medium)
                    .lineLimit(1)
                Spacer(minLength: 6)
                VerdictBadge(session: session)
            }
            Text([inRole ? session.stage : nil, session.date, formatDuration(session.durationS),
                  session.isDeleted ? "\(session.deletedDaysLeft ?? 0)d left" : nil, session.archived ? "archived" : nil]
                .compactMap { $0 }.joined(separator: " · "))
                .font(.caption)
                .foregroundStyle(.secondary)
            if let outcome = session.outcomeLabel {
                Label(outcome, systemImage: "flag")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 3)
    }
}

/// A setup problem from `ic doctor --json`, e.g. "not signed in".
struct ProblemRow: View {
    let problem: String

    var body: some View {
        Label {
            Text(problem).font(.caption)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.orange)
        }
        .textSelection(.enabled)
    }
}

/// Runs `action` every time the window holding this view comes to the front, not just the first
/// time the view appears. The menu-bar panel keeps its view alive between openings, so a plain
/// `.task` would show whatever was true when it first opened.
struct RefreshWhenShown: ViewModifier {
    let action: () async -> Void
    @State private var window: NSWindow?

    func body(content: Content) -> some View {
        content
            .background(WindowReader(window: $window))
            .task { await action() }
            .onReceive(NotificationCenter.default.publisher(for: NSWindow.didBecomeKeyNotification)) { note in
                guard let window, note.object as? NSWindow === window else { return }
                Task { await action() }
            }
    }
}

extension View {
    func refreshWhenShown(_ action: @escaping () async -> Void) -> some View {
        modifier(RefreshWhenShown(action: action))
    }
}

/// Finds the window a SwiftUI view is in.
private struct WindowReader: NSViewRepresentable {
    @Binding var window: NSWindow?

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { window = view.window }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        if view.window !== window {
            DispatchQueue.main.async { window = view.window }
        }
    }
}
