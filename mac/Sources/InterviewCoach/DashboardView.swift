import Charts
import InterviewCoachKit
import SwiftUI

/// Conversations come first. The numbers live in the margin rather than leading the page.
struct DashboardView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    @State private var selectedActivity: String?
    @State private var period: AnalyticsPeriod = .quarter

    var body: some View {
        GeometryReader { geometry in
            let data = LibraryAnalytics(library: model.library, period: period)
            ScrollView {
                VStack(alignment: .leading, spacing: 28) {
                    heading
                    CoachRule()
                    if model.sessions.isEmpty {
                        firstInterview
                    } else if data.sessions.isEmpty {
                        emptyRange
                    } else {
                        if geometry.size.width >= 730 {
                            HStack(alignment: .top, spacing: 30) {
                                conversations(data).frame(maxWidth: .infinity, alignment: .leading)
                                Rectangle().fill(CoachTheme.line).frame(width: 1).accessibilityHidden(true)
                                margin(data).frame(width: 200)
                            }
                            .fixedSize(horizontal: false, vertical: true)
                        } else {
                            conversations(data)
                            CoachRule()
                            margin(data)
                        }
                        CoachRule()
                        activity(data)
                        Text("Local library. Archived and deleted interviews are left out.")
                            .font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
                    }
                }
                .frame(maxWidth: 1000, alignment: .leading)
                .padding(geometry.size.width < 640 ? 26 : 40)
                .frame(maxWidth: .infinity)
            }
            .background(CoachTheme.surface)
        }
        .onChange(of: period) { selectedActivity = nil }
    }

    private var heading: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack(alignment: .firstTextBaseline) {
                Text(Date().formatted(.dateTime.month(.wide).year()))
                    .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                Spacer()
                Picker("Time period", selection: $period) {
                    ForEach(AnalyticsPeriod.allCases) { Text($0.label).tag($0) }
                }
                .labelsHidden().pickerStyle(.menu).fixedSize()
                .accessibilityLabel("Notebook time period")
            }
            Text("Interview notebook")
                .font(CoachTheme.editorial(36)).tracking(-1.3).foregroundStyle(CoachTheme.ink)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func conversations(_ data: LibraryAnalytics) -> some View {
        VStack(alignment: .leading, spacing: 24) {
            if let latest = data.sessions.first {
                VStack(alignment: .leading, spacing: 12) {
                    HStack {
                        Text("Last conversation").font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                        Spacer()
                        Text(shortDate(latest.createdAt)).font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                    }
                    Text(latest.company ?? "Company not set")
                        .font(.system(size: 12, weight: .medium)).foregroundStyle(CoachTheme.accent)
                    Text(latest.title)
                        .font(CoachTheme.editorial(25)).tracking(-0.5).foregroundStyle(CoachTheme.ink)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 12) {
                        Text(latest.stage ?? "Interview").font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                        Text(formatDuration(latest.durationS)).font(.system(size: 11, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                        Spacer()
                        VerdictBadge(session: latest)
                    }
                    Button { model.selection = latest.id } label: {
                        HStack(spacing: 8) {
                            Text(latest.isAnalyzed || latest.status == "analyzed" ? "Read the review" : "Open interview")
                            Image(systemName: "arrow.right").font(.system(size: 10))
                        }
                        .font(.system(size: 12, weight: .medium))
                    }
                    .buttonStyle(CoachTextButtonStyle()).padding(.top, 4)
                }
            }
            let earlier = Array(data.sessions.dropFirst().prefix(3))
            if !earlier.isEmpty {
                VStack(alignment: .leading, spacing: 0) {
                    Text("Earlier conversations").font(.system(size: 11)).foregroundStyle(CoachTheme.muted).padding(.bottom, 12)
                    ForEach(earlier) { session in
                        CoachRule()
                        Button { model.selection = session.id } label: {
                            HStack(alignment: .firstTextBaseline, spacing: 14) {
                                Text(shortDate(session.createdAt))
                                    .font(.system(size: 10, design: .monospaced))
                                    .foregroundStyle(CoachTheme.muted).frame(width: 46, alignment: .leading)
                                VStack(alignment: .leading, spacing: 5) {
                                    Text(session.title).font(.system(size: 12, weight: .medium)).foregroundStyle(CoachTheme.ink).lineLimit(1)
                                    Text(session.company ?? "Company not set").font(.system(size: 11)).foregroundStyle(CoachTheme.muted).lineLimit(1)
                                }
                                Spacer(minLength: 6)
                                Image(systemName: "arrow.right").font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
                            }
                            .padding(.vertical, 13).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain).help("Open \(session.title)")
                    }
                }
            }
        }
    }

    private func margin(_ data: LibraryAnalytics) -> some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 15) {
                Text("The record").font(CoachTheme.editorial(18)).foregroundStyle(CoachTheme.ink)
                fact("Conversations", value: "\(data.sessions.count)")
                fact("Reviews", value: "\(data.reviewed)")
                fact("Recorded time", value: durationLabel(data.duration))
                fact("Active roles", value: "\(data.activeRoles)")
            }
            CoachRule()
            VStack(alignment: .leading, spacing: 13) {
                Text("What happened next").font(CoachTheme.editorial(18)).foregroundStyle(CoachTheme.ink)
                if data.recordedOutcomes == 0 {
                    Text("No results recorded yet.").font(.system(size: 12)).foregroundStyle(CoachTheme.muted)
                } else {
                    ForEach(data.outcomes.filter { $0.count > 0 }) { outcome in
                        fact(outcome.label, value: "\(outcome.count)")
                    }
                }
                Text("\(data.awaitingOutcome) without a recorded result.")
                    .font(CoachTheme.editorial(13)).italic().foregroundStyle(CoachTheme.muted)
                Text("Recorded results, rather than predictions.")
                    .font(.system(size: 10)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private func fact(_ label: String, value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Text(label).font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
            Spacer()
            Text(value).font(.system(size: 13, weight: .medium, design: .monospaced)).foregroundStyle(CoachTheme.ink)
        }
        .accessibilityElement(children: .combine)
    }

    private func activity(_ data: LibraryAnalytics) -> some View {
        VStack(alignment: .leading, spacing: 17) {
            HStack(alignment: .firstTextBaseline) {
                Text("Conversations over time").font(CoachTheme.editorial(19)).foregroundStyle(CoachTheme.ink)
                Spacer()
                Text(data.usesMonthlyBuckets ? "By month" : "By week").font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
            }
            Chart(data.activity) { bucket in
                BarMark(x: .value("Period starting", activityLabel(bucket, monthly: data.usesMonthlyBuckets)),
                        y: .value("Interviews", bucket.count), width: .ratio(0.38))
                    .foregroundStyle(CoachTheme.accent)
                    .annotation(position: .top) {
                        if selectedActivity == activityLabel(bucket, monthly: data.usesMonthlyBuckets) {
                            Text("\(bucket.count)").font(.system(size: 11, design: .monospaced)).foregroundStyle(CoachTheme.accent)
                        }
                    }
                    .accessibilityLabel("Period starting \(bucket.date.formatted(date: .abbreviated, time: .omitted))")
                    .accessibilityValue("\(bucket.count) interviews")
            }
            .chartXSelection(value: $selectedActivity)
            .chartYScale(domain: 0...max(3, (data.activity.map(\.count).max() ?? 0) + 1))
            .chartYAxis {
                AxisMarks(position: .leading, values: .stride(by: max(1, Double(data.activity.map(\.count).max() ?? 0) / 3).rounded(.up))) {
                    AxisGridLine().foregroundStyle(CoachTheme.line.opacity(0.5))
                    AxisValueLabel().font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                }
            }
            .chartXAxis {
                AxisMarks(values: data.activity.enumerated().filter { $0.offset % max(1, Int(ceil(Double(data.activity.count) / 5))) == 0 }.map { activityLabel($0.element, monthly: data.usesMonthlyBuckets) }) {
                    AxisValueLabel().font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                }
            }
            .frame(height: 115)
        }
    }

    private var emptyRange: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("No interviews in this period.").font(CoachTheme.editorial(25)).foregroundStyle(CoachTheme.ink)
            Text("There are no interviews in this period. Choose a wider range to see earlier conversations.")
                .font(.system(size: 13)).foregroundStyle(CoachTheme.muted).frame(maxWidth: 390, alignment: .leading)
        }
        .padding(.vertical, 34)
    }

    private var firstInterview: some View {
        VStack(alignment: .leading, spacing: 24) {
            BrandMark(size: 96)
            Text("Your notebook is empty.").font(CoachTheme.editorial(28)).foregroundStyle(CoachTheme.ink)
            Text("An interview is easier to learn from when you can return to it. Record a call or bring in an existing recording; your review and preparation will live here.")
                .font(.system(size: 13)).lineSpacing(5).foregroundStyle(CoachTheme.muted)
                .frame(maxWidth: 390, alignment: .leading)
            HStack(spacing: 22) {
                Button("Record interview") { openWindow(id: "record") }.buttonStyle(CoachPrimaryButtonStyle())
                Button("Import a recording") { model.importRecording() }.buttonStyle(CoachTextButtonStyle())
            }
            .disabled(model.phase.isBusy)
            Text("Audio stays on your Mac. Your chosen provider writes the review.")
                .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
        }
        .padding(.vertical, 24)
    }

    private func activityLabel(_ bucket: LibraryAnalytics.Activity, monthly: Bool) -> String {
        bucket.date.formatted(monthly ? .dateTime.month(.abbreviated).year(.twoDigits) : .dateTime.month(.abbreviated).day())
    }

    private func shortDate(_ iso: String) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        guard let date = ISO8601DateFormatter().date(from: iso) ?? formatter.date(from: iso) else { return String(iso.prefix(10)) }
        return date.formatted(.dateTime.month(.abbreviated).day())
    }

    private func durationLabel(_ seconds: Double) -> String {
        let minutes = Int(seconds / 60)
        return minutes >= 60 ? "\(minutes / 60)h \(minutes % 60)m" : "\(minutes)m"
    }
}
