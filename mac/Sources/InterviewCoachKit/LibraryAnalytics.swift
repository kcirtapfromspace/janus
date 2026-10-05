import Foundation

public enum AnalyticsPeriod: String, CaseIterable, Identifiable {
    case month, quarter, all
    public var id: String { rawValue }
    public var label: String {
        switch self { case .month: "30 days"; case .quarter: "90 days"; case .all: "All time" }
    }
    public var days: Int? {
        switch self { case .month: 30; case .quarter: 90; case .all: nil }
    }
}

/// Aggregates local library facts. Recorded outcomes are kept separate from AI verdicts.
public struct LibraryAnalytics {
    public struct Activity: Identifiable {
        public let date: Date
        public let count: Int
        public var id: Date { date }
    }
    public struct OutcomeCount: Identifiable {
        public let value: String
        public let label: String
        public let count: Int
        public var id: String { value }
    }
    public let sessions: [SessionSummary]
    public let reviewed: Int
    public let duration: Double
    public let activeRoles: Int
    public let activity: [Activity]
    public let usesMonthlyBuckets: Bool
    public let outcomes: [OutcomeCount]
    public let awaitingOutcome: Int
    public let recordedOutcomes: Int
    public let start: Date
    public let end: Date

    public init(library: Library, period: AnalyticsPeriod, now: Date = Date(), calendar: Calendar = .current) {
        let formatter = ISO8601DateFormatter()
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        let today = calendar.startOfDay(for: now)
        let dated = library.sessions.filter { !$0.isDeleted && !$0.archived }.compactMap { session -> (SessionSummary, Date)? in
            guard let date = formatter.date(from: session.createdAt) ?? fractional.date(from: session.createdAt), date <= now else { return nil }
            return (session, date)
        }
        // Includes today: 30 days means today plus the previous 29 calendar days.
        let rangeStart = period.days.flatMap { calendar.date(byAdding: .day, value: -($0 - 1), to: today) }
            ?? calendar.startOfDay(for: dated.map(\.1).min() ?? today)
        start = rangeStart
        end = calendar.date(byAdding: .day, value: 1, to: today) ?? now
        let scoped = dated.filter { $0.1 >= rangeStart }.sorted { $0.1 > $1.1 }
        let scopedSessions = scoped.map(\.0)
        sessions = scopedSessions
        reviewed = sessions.filter { $0.status == "analyzed" || $0.reportPath != nil }.count
        duration = sessions.reduce(0) { $0 + max(0, $1.durationS ?? 0) }
        let roleIDs = Set(sessions.compactMap(\.roleId))
        activeRoles = library.roles.filter { roleIDs.contains($0.id) && !$0.archived && $0.status == "interviewing" }.count
        outcomes = outcomeChoices.filter { $0.value != "pending" }.map { choice in
            OutcomeCount(value: choice.value, label: choice.label, count: scopedSessions.filter { $0.outcome == choice.value }.count)
        }
        recordedOutcomes = outcomes.reduce(0) { $0 + $1.count }
        awaitingOutcome = sessions.count - recordedOutcomes
        // Buckets stay bounded for long libraries, and zero buckets keep the scale honest.
        let span = calendar.dateComponents([.day], from: start, to: today).day ?? 0
        usesMonthlyBuckets = span > 180
        let component: Calendar.Component = span > 180 ? .month : .weekOfYear
        let first = calendar.dateInterval(of: component, for: start)?.start ?? start
        let last = calendar.dateInterval(of: component, for: today)?.start ?? today
        let counts = Dictionary(grouping: scoped) { calendar.dateInterval(of: component, for: $0.1)?.start ?? $0.1 }
        var buckets: [Activity] = []
        var cursor = first
        while cursor <= last {
            buckets.append(Activity(date: cursor, count: counts[cursor]?.count ?? 0))
            guard let next = calendar.date(byAdding: component, value: 1, to: cursor), next > cursor else { break }
            cursor = next
        }
        activity = buckets
    }
}
