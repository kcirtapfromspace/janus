import Foundation
@testable import InterviewCoachKit
import XCTest

/// `ic session N` output from a real staged run (paths sanitized), decoded the way the app does.
final class SessionDetailTests: XCTestCase {
    private func fixture() throws -> SessionDetail {
        let url = try XCTUnwrap(Bundle.module.url(forResource: "session", withExtension: "json", subdirectory: "Fixtures"))
        return try ICClient.decode(SessionDetail.self, from: Data(contentsOf: url))
    }

    func testDecodesAllFourStages() throws {
        let detail = try fixture()
        XCTAssertEqual(detail.stages.map(\.step), [.recording, .transcript, .report, .next])
        XCTAssertTrue(detail.stages.allSatisfy { $0.status == .done })
        XCTAssertNil(detail.firstOutOfDate)
        XCTAssertFalse(detail.isBusy)
        XCTAssertTrue(detail.stages.allSatisfy(\.hasResult))
        XCTAssertTrue(detail.stages.allSatisfy { $0.warnings.isEmpty })
    }

    func testDecodesTranscriptReportsAndNextSteps() throws {
        let detail = try fixture()
        XCTAssertEqual(detail.turns.count, 16)
        XCTAssertTrue(detail.turns.contains(where: \.isYou))
        XCTAssertEqual(detail.reports.count, 1)
        XCTAssertTrue(detail.reports[0].isCurrent)
        let next = try XCTUnwrap(detail.nextSteps)
        XCTAssertFalse(next.plan.nextRoundPrep.isEmpty)
        XCTAssertFalse(next.plan.practicePlan.isEmpty)
        XCTAssertEqual(detail.audio.tracks.map(\.name), ["mic", "system"])
        XCTAssertNotNil(detail.audio.listenPath)
        XCTAssertNil(detail.audio.videoPath, "a session from before video recording has none")
    }

    func testDecodesTheCallsVideo() throws {
        let json = #"{"listen_path": "/s/listen.m4a", "video_path": "/s/video.mov", "tracks": [], "warnings": []}"#
        let audio = try ICClient.decode(SessionDetail.Audio.self, from: Data(json.utf8))
        XCTAssertEqual(audio.videoPath, "/s/video.mov")
    }

    func testTimestampsParse() {
        XCTAssertEqual(seconds(fromTimestamp: "00:01:05"), 65)
        XCTAssertEqual(seconds(fromTimestamp: "1:02:03"), 3723)
    }
}
