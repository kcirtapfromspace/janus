import Charts
import InterviewCoachKit
import SwiftUI

/// The notebook's account of how you're doing across interviews: where you stand overall, which
/// areas are getting better or worse, and the coaching that keeps coming up. ic decides every
/// direction (`ic trends`); this only lays it out.
struct ProgressSection: View {
    let trends: Trends
    /// Room for the overall chart and its margin side by side.
    let wide: Bool
    /// Room for the areas in two columns.
    let columns: Bool
    let periodLabel: String
    @State private var selected: Int?

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            summary
            if !trends.interviews.isEmpty {
                if wide && trends.overall?.points.isEmpty == false {
                    HStack(alignment: .top, spacing: 30) {
                        overall.frame(maxWidth: .infinity, alignment: .leading)
                        Rectangle().fill(CoachTheme.line).frame(width: 1).accessibilityHidden(true)
                        notes.frame(width: 200)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                } else {
                    if trends.overall?.points.isEmpty == false {
                        overall
                        CoachRule()
                    }
                    notes
                }
                CoachRule()
                areas
                footnote
            }
        }
    }

    private var summary: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                Text("How you’re doing").font(CoachTheme.editorial(19)).foregroundStyle(CoachTheme.ink)
                Spacer()
                Text(scopeLabel).font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
            }
            Text(trends.summary.headline)
                .font(CoachTheme.editorial(25)).tracking(-0.5).foregroundStyle(CoachTheme.ink)
                .fixedSize(horizontal: false, vertical: true)
            if let detail = trends.summary.detail {
                Text(detail).font(.system(size: 13)).lineSpacing(4).foregroundStyle(CoachTheme.ink)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    private var scopeLabel: String {
        let n = trends.interviews.count
        return "\(n) reviewed interview\(n == 1 ? "" : "s") · \(periodLabel)"
    }

    // MARK: Overall

    @ViewBuilder private var overall: some View {
        if let measure = trends.overall, !measure.points.isEmpty {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .firstTextBaseline, spacing: 10) {
                    Text(measure.recent.map(measure.format) ?? "–")
                        .font(CoachTheme.editorial(36)).tracking(-1).foregroundStyle(CoachTheme.ink)
                    Text(measure.direction == "too_few" ? "out of 5 so far" : "out of 5 lately")
                        .font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
                    Spacer()
                    DirectionLabel(measure: measure)
                }
                .accessibilityElement(children: .combine)
                Chart {
                    ForEach(points(measure), id: \.0) { position, value in
                        LineMark(x: .value("Interview", position), y: .value("Overall score", value))
                            .foregroundStyle(CoachTheme.accent)
                        PointMark(x: .value("Interview", position), y: .value("Overall score", value))
                            .foregroundStyle(CoachTheme.accent).symbolSize(position == selected ? 60 : 22)
                            .accessibilityLabel(interviewLabel(position))
                            .accessibilityValue("\(measure.format(value)) out of 5")
                    }
                    // The two means being compared, each over the interviews it's the mean of.
                    let all = points(measure)
                    if let earlier = measure.earlier, let recent = measure.recent, measure.earlierCount > 0,
                       all.count == measure.earlierCount + measure.recentCount {
                        RuleMark(xStart: .value("From", all[0].0), xEnd: .value("To", all[measure.earlierCount - 1].0),
                                 y: .value("Before", earlier))
                            .foregroundStyle(CoachTheme.muted.opacity(0.7)).lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                        RuleMark(xStart: .value("From", all[measure.earlierCount].0), xEnd: .value("To", all[all.count - 1].0),
                                 y: .value("Lately", recent))
                            .foregroundStyle(CoachTheme.accent.opacity(0.6)).lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                    }
                    if let selected, let point = points(measure).first(where: { $0.0 == selected }) {
                        RuleMark(x: .value("Interview", point.0))
                            .foregroundStyle(CoachTheme.line)
                            .annotation(position: .top, alignment: .center, spacing: 4,
                                        overflowResolution: .init(x: .fit(to: .chart), y: .disabled)) {
                                VStack(spacing: 2) {
                                    Text(interviewLabel(point.0)).font(.system(size: 10)).foregroundStyle(CoachTheme.ink)
                                    Text(measure.format(point.1)).font(.system(size: 11, design: .monospaced)).foregroundStyle(CoachTheme.accent)
                                }
                                .padding(.horizontal, 6).padding(.vertical, 3)
                                .background(CoachTheme.surface, in: RoundedRectangle(cornerRadius: 4))
                            }
                    }
                }
                .chartXSelection(value: $selected)
                .chartXScale(domain: 0...max(1, trends.interviews.count - 1))
                .chartYScale(domain: 1...5)
                .chartYAxis {
                    AxisMarks(position: .leading, values: [1, 3, 5]) {
                        AxisGridLine().foregroundStyle(CoachTheme.line.opacity(0.5))
                        AxisValueLabel().font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                    }
                }
                .chartXAxis {
                    AxisMarks(values: axisPositions) { value in
                        AxisValueLabel {
                            if let position = value.as(Int.self) {
                                Text(shortDate(trends.interviews[position].createdAt))
                                    .font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                            }
                        }
                    }
                }
                // Grows to the margin's height beside it, never shorter than this.
                .frame(minHeight: 120, maxHeight: wide ? .infinity : 120)
                Text(measure.earlierCount > 0
                     ? "The mean of each review’s rubric scores. Dashed lines: your latest \(measure.recentCount), and the \(measure.earlierCount) before."
                     : "The mean of each review’s rubric scores.")
                    .font(.system(size: 10)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
    }

    /// (position along the reviewed interviews, value), oldest first.
    private func points(_ measure: Trends.Measure) -> [(Int, Double)] {
        measure.points.compactMap { point in trends.position(of: point.sessionId).map { ($0, point.value) } }
    }

    private var axisPositions: [Int] {
        let n = trends.interviews.count
        guard n > 1 else { return n == 1 ? [0] : [] }
        let step = max(1, Int((Double(n - 1) / 4).rounded(.up)))
        var positions = Array(stride(from: 0, to: n - 1, by: step))
        positions.append(n - 1)
        return positions
    }

    private func interviewLabel(_ position: Int) -> String {
        let interview = trends.interviews[position]
        return "\(shortDate(interview.createdAt)) · \(interview.company ?? interview.title)"
    }

    // MARK: The margin

    private var notes: some View {
        VStack(alignment: .leading, spacing: 20) {
            if let strongest = trends.summary.strongest, let weakest = trends.summary.weakest {
                VStack(alignment: .leading, spacing: 13) {
                    Text("Lately").font(CoachTheme.editorial(18)).foregroundStyle(CoachTheme.ink)
                    standing("Strongest", strongest)
                    standing("Most room to grow", weakest)
                }
                CoachRule()
            }
            VStack(alignment: .leading, spacing: 12) {
                Text("Keeps coming up").font(CoachTheme.editorial(18)).foregroundStyle(CoachTheme.ink)
                if trends.summary.recurring.isEmpty {
                    Text("No coaching has come up in more than one review yet.")
                        .font(.system(size: 12)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
                } else {
                    ForEach(trends.summary.recurring) { theme in
                        VStack(alignment: .leading, spacing: 3) {
                            Text(theme.title).font(.system(size: 12, weight: .medium)).foregroundStyle(CoachTheme.ink)
                                .fixedSize(horizontal: false, vertical: true)
                            Text(theme.inLatest ? "In \(theme.count) reviews, including your latest."
                                                : "In \(theme.count) reviews; not since \(shortDate(theme.lastSeen)).")
                                .font(.system(size: 10)).foregroundStyle(theme.inLatest ? CoachTheme.muted : CoachTheme.accent)
                        }
                        .accessibilityElement(children: .combine)
                    }
                }
            }
        }
    }

    private func standing(_ label: String, _ standing: Trends.Standing) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(label).font(.system(size: 11)).foregroundStyle(CoachTheme.muted)
            HStack(alignment: .firstTextBaseline) {
                Text(standing.label).font(.system(size: 12, weight: .medium)).foregroundStyle(CoachTheme.ink)
                Spacer()
                Text(String(format: "%.1f", standing.value)).font(.system(size: 13, weight: .medium, design: .monospaced))
                    .foregroundStyle(CoachTheme.ink)
            }
        }
        .accessibilityElement(children: .combine)
    }

    // MARK: Areas

    @ViewBuilder private var areas: some View {
        let groups = Trends.groups.filter { !measured($0.id).isEmpty }
        if columns && groups.count > 1 {
            HStack(alignment: .top, spacing: 30) {
                VStack(alignment: .leading, spacing: 22) { ForEach(groups.prefix(1), id: \.id) { group($0) } }
                    .frame(maxWidth: .infinity, alignment: .leading)
                VStack(alignment: .leading, spacing: 22) { ForEach(groups.dropFirst(), id: \.id) { group($0) } }
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            VStack(alignment: .leading, spacing: 22) { ForEach(groups, id: \.id) { group($0) } }
        }
    }

    private func group(_ group: (id: String, title: String)) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(group.title).font(CoachTheme.editorial(16)).foregroundStyle(CoachTheme.ink).padding(.bottom, 6)
            ForEach(measured(group.id)) { measure in
                CoachRule()
                AreaRow(measure: measure, points: points(measure), count: trends.interviews.count)
            }
        }
    }

    /// A group's areas that at least one review measured.
    private func measured(_ group: String) -> [Trends.Measure] {
        trends.measures(in: group).filter { !$0.points.isEmpty }
    }

    private var footnote: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Each area compares your latest interviews (up to three, never more than half) with the ones before. A move counts once it’s at least half a point on the 1–5 scores, or 15 points on the answer habits; anything smaller is steady. Practice, archived and deleted interviews are left out.")
            if trends.models.count > 1 {
                Text("These reviews were written by \(trends.models.count) different models, which can score a little differently.")
            }
        }
        .font(.system(size: 10)).foregroundStyle(CoachTheme.muted).fixedSize(horizontal: false, vertical: true)
    }
}

/// One area: its line across your interviews, where it is lately, and which way it's going.
private struct AreaRow: View {
    let measure: Trends.Measure
    let points: [(Int, Double)]
    let count: Int

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(measure.label).font(.system(size: 12)).foregroundStyle(CoachTheme.ink)
                    .lineLimit(2).fixedSize(horizontal: false, vertical: true)
                if let note = measure.unitNote {
                    Text(note).font(.system(size: 10)).foregroundStyle(CoachTheme.muted)
                        .lineLimit(2).fixedSize(horizontal: false, vertical: true)
                }
            }
            .layoutPriority(1)
            Spacer(minLength: 6)
            sparkline.frame(width: 64, height: 22)
            Text(measure.recent.map(measure.format) ?? "–")
                .font(.system(size: 12, weight: .medium, design: .monospaced)).foregroundStyle(CoachTheme.ink)
                .frame(width: 50, alignment: .trailing)
            VStack(alignment: .leading, spacing: 2) {
                DirectionLabel(measure: measure)
                if let earlier = measure.earlier {
                    Text("from \(measure.format(earlier))").font(.system(size: 10, design: .monospaced)).foregroundStyle(CoachTheme.muted)
                }
            }
            .frame(width: 96, alignment: .leading)
        }
        .padding(.vertical, 8)
        .help(help)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(measure.label)
        .accessibilityValue(help)
    }

    private var sparkline: some View {
        Chart {
            ForEach(points, id: \.0) { position, value in
                LineMark(x: .value("Interview", position), y: .value(measure.label, value))
                    .foregroundStyle(CoachTheme.muted.opacity(0.8)).lineStyle(StrokeStyle(lineWidth: 1.2))
            }
            if let last = points.last {
                PointMark(x: .value("Interview", last.0), y: .value(measure.label, last.1))
                    .foregroundStyle(DirectionLabel.color(measure.direction)).symbolSize(18)
            }
        }
        .chartXScale(domain: 0...max(1, count - 1))
        .chartYScale(domain: measure.scale)
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .accessibilityHidden(true)
    }

    private var help: String {
        guard let recent = measure.recent else { return "Not measured yet." }
        guard let earlier = measure.earlier else {
            let n = measure.recentCount
            return "\(measure.format(recent)) across \(n == 1 ? "your one interview" : "your \(n) interviews") so far. \(measure.directionLabel)."
        }
        return "\(measure.format(recent)) across your latest \(measure.recentCount), \(measure.format(earlier)) across the \(measure.earlierCount) before. \(measure.directionLabel)."
    }
}

/// Getting better, slipping, steady: text, coloured only when it's a judgement.
private struct DirectionLabel: View {
    let measure: Trends.Measure

    static func color(_ direction: String) -> Color {
        switch direction {
        case "improving": CoachTheme.accent
        case "slipping": CoachTheme.alert
        default: CoachTheme.muted
        }
    }

    var body: some View {
        HStack(spacing: 4) {
            if let arrow { Image(systemName: arrow).font(.system(size: 8, weight: .semibold)).accessibilityHidden(true) }
            Text(measure.directionLabel).font(.system(size: 11, weight: measure.direction == "improving" || measure.direction == "slipping" ? .medium : .regular))
        }
        .foregroundStyle(Self.color(measure.direction))
    }

    /// Which way the number moved (for fewer fillers, down is getting better).
    private var arrow: String? {
        guard let change = measure.change, ["improving", "slipping", "up", "down"].contains(measure.direction) else { return nil }
        return change > 0 ? "arrow.up" : "arrow.down"
    }
}

private func shortDate(_ iso: String) -> String {
    let fractional = ISO8601DateFormatter()
    fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    guard let date = ISO8601DateFormatter().date(from: iso) ?? fractional.date(from: iso) else { return String(iso.prefix(10)) }
    return date.formatted(.dateTime.month(.abbreviated).day())
}
