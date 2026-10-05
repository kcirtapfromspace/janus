import Foundation
@testable import InterviewCoachKit
import XCTest

final class LibraryAnalyticsTests: XCTestCase {
    private var calendar: Calendar {
        var value = Calendar(identifier: .gregorian)
        value.timeZone = TimeZone(secondsFromGMT: 0)!
        return value
    }
    private let now = ISO8601DateFormatter().date(from: "2026-10-04T12:00:00Z")!
    private func fixture() throws -> Library {
        let url = Bundle.module.url(forResource: "library", withExtension: "json", subdirectory: "Fixtures")!
        return try ICClient.decode(Library.self, from: Data(contentsOf: url))
    }
    private func session(id: Int, date: String, outcome: String? = nil, status: String = "analyzed") throws -> SessionSummary {
        let object: [String: Any] = ["id": id, "created_at": date, "title": "Interview", "status": status,
                                   "mode": "dual", "dir": "/fixture", "duration_s": 600, "archived": false,
                                   "outcome": outcome as Any? ?? NSNull()]
        return try ICClient.decode(SessionSummary.self, from: JSONSerialization.data(withJSONObject: object))
    }

    func testFactsExcludeDeletedAndArchivedAndKeepPredictionsSeparate() throws {
        let data = LibraryAnalytics(library: try fixture(), period: .quarter, now: now, calendar: calendar)
        XCTAssertEqual(data.sessions.count, 6)
        XCTAssertEqual(data.reviewed, 6, "analyzed status counts even if an older report path is absent")
        XCTAssertEqual(data.duration, 12131)
        XCTAssertEqual(data.activeRoles, 1)
        XCTAssertEqual(data.recordedOutcomes, 2)
        XCTAssertEqual(data.awaitingOutcome, 4)
        XCTAssertEqual(data.outcomes.first { $0.value == "offer" }?.count, 1)
        XCTAssertEqual(data.outcomes.first { $0.value == "advanced" }?.count, 1, "strong predictions do not count as real advancement")
        XCTAssertEqual(data.activity.reduce(0) { $0 + $1.count }, data.sessions.count)
        XCTAssertTrue(data.activity.contains { $0.count == 0 })
    }

    func testPeriodBoundariesFractionalDatesAndFutureDates() throws {
        let sessions = try [
            session(id: 1, date: "2026-09-05T00:00:00Z"),
            session(id: 2, date: "2026-09-04T23:59:59Z"),
            session(id: 3, date: "2026-10-04T11:59:59.123Z"),
            session(id: 4, date: "2026-10-04T12:00:01Z"),
            session(id: 5, date: "invalid")
        ]
        let data = LibraryAnalytics(library: Library(sessions: sessions), period: .month, now: now, calendar: calendar)
        XCTAssertEqual(data.sessions.map(\.id), [3, 1])
        XCTAssertEqual(data.duration, 1200)
        XCTAssertEqual(data.activity.reduce(0) { $0 + $1.count }, 2)
    }

    func testEmptyRangeDoesNotInventPercentagesOrResults() {
        let data = LibraryAnalytics(library: Library(), period: .month, now: now, calendar: calendar)
        XCTAssertEqual(data.sessions.count, 0)
        XCTAssertEqual(data.duration, 0)
        XCTAssertEqual(data.recordedOutcomes, 0)
        XCTAssertEqual(data.awaitingOutcome, 0)
        XCTAssertTrue(data.activity.allSatisfy { $0.count == 0 })
        XCTAssertFalse(data.usesMonthlyBuckets)
    }

    func testAllTimeBoundsLongHistoryIntoMonthlyBuckets() throws {
        let library = Library(sessions: try [session(id: 1, date: "2020-01-01T12:00:00Z"), session(id: 2, date: "2026-10-04T10:00:00Z")])
        let data = LibraryAnalytics(library: library, period: .all, now: now, calendar: calendar)
        XCTAssertTrue(data.usesMonthlyBuckets)
        XCTAssertEqual(data.activity.count, 82)
        XCTAssertEqual(data.activity.reduce(0) { $0 + $1.count }, 2)
    }
}
