import Foundation
@testable import InterviewCoachKit
import XCTest

/// `Fixtures/trends.json` is real `ic trends` output (regenerate: `cargo test --test trends -- --ignored`).
final class TrendsTests: XCTestCase {
    private func fixture() throws -> Trends {
        let url = Bundle.module.url(forResource: "trends", withExtension: "json", subdirectory: "Fixtures")!
        return try ICClient.decode(Trends.self, from: Data(contentsOf: url))
    }

    func testDecodesWhatICWrites() throws {
        let trends = try fixture()
        XCTAssertEqual(trends.days, 90)
        XCTAssertEqual(trends.interviews.count, 6)
        XCTAssertEqual(trends.overall?.direction, "steady")
        XCTAssertEqual(trends.measures.first { $0.id == "structure" }?.directionLabel, "Getting better")
        XCTAssertEqual(trends.measures.first { $0.id == "composure" }?.directionLabel, "Slipping")
        XCTAssertEqual(trends.summary.slipping, ["Composure"])
        XCTAssertTrue(trends.summary.recurring.contains { !$0.inLatest }, "coaching that stopped coming up is kept")
        XCTAssertEqual(trends.models.count, 2)
        // Every group the dashboard lays out is one ic writes, and every point is one of its interviews.
        XCTAssertEqual(Set(trends.measures.map(\.group)).subtracting(["overall"]), Set(Trends.groups.map(\.id)))
        for measure in trends.measures {
            XCTAssertTrue(measure.points.allSatisfy { trends.position(of: $0.sessionId) != nil }, measure.id)
        }
    }

    func testFormatsEachUnit() throws {
        let measures = Dictionary(try fixture().measures.map { ($0.unit, $0) }, uniquingKeysWith: { first, _ in first })
        XCTAssertEqual(measures["score"]?.format(3.46), "3.5")
        XCTAssertEqual(measures["percent"]?.format(0.916), "92%")
        XCTAssertEqual(measures["seconds"]?.format(44), "44s")
        XCTAssertEqual(measures["seconds"]?.format(95), "1m 35s")
        XCTAssertEqual(measures["outlook"]?.format(1), "+1.0")
        XCTAssertEqual(measures["per_100_words"]?.unitNote, "per 100 of your words")
        XCTAssertEqual(measures["score"]?.scale, 1...5)
    }

    func testTooFewSaysHowManyMore() throws {
        let json = """
        {"id": "structure", "label": "Structure", "group": "rubric", "unit": "score", "better": "higher", "band": 0.5,
         "points": [{"session_id": 1, "value": 3}], "recent": 3, "recent_count": 1, "earlier": null, "earlier_count": 0,
         "change": null, "direction": "too_few", "needed": 2}
        """
        let measure = try ICClient.decode(Trends.Measure.self, from: Data(json.utf8))
        XCTAssertEqual(measure.directionLabel, "2 more to tell")
    }
}
