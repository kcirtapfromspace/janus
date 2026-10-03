@testable import ICRecorderCore
import XCTest

final class ContinuityTests: XCTestCase {
    private func stats(_ name: String, seconds: Double, gaps: Double = 0, events: Int = 0) -> TrackWriter.Stats {
        let rate = 48000.0
        return TrackWriter.Stats(
            file: "\(name).wav", sampleRate: rate, channels: 1, frames: Int64(seconds * rate), durationSeconds: seconds,
            capturedFrames: Int64((seconds - gaps) * rate), firstBufferHostTime: nil, startOffsetSeconds: 0,
            leadingPadFrames: 0, droppedLeadingFrames: 0, gapFillFrames: Int64(gaps * rate), gapFillEvents: events,
            maxAheadSeconds: 0, rms: 0.05, peak: 0.5, writeError: nil
        )
    }

    /// The real failure: the mic stopped 25 s into a 42-minute interview.
    func testMicThatStoppedEarlyIsFlagged() {
        let issues = RecordingSession.continuityWarnings(["mic": stats("mic", seconds: 25.2), "system": stats("system", seconds: 2531.8)])
        XCTAssertEqual(issues.map(\.code), ["mic_stopped"])
        XCTAssertTrue(issues[0].message.contains("stopped recording at 0:25"), issues[0].message)
        XCTAssertTrue(issues[0].message.contains("41:46 before the end"), issues[0].message)
    }

    func testSystemThatStoppedEarlyIsFlagged() {
        let issues = RecordingSession.continuityWarnings(["mic": stats("mic", seconds: 600), "system": stats("system", seconds: 120)])
        XCTAssertEqual(issues.map(\.code), ["system_stopped"])
    }

    func testTracksEndingTogetherAreFine() {
        let issues = RecordingSession.continuityWarnings(["mic": stats("mic", seconds: 598), "system": stats("system", seconds: 600)])
        XCTAssertEqual(issues, [])
    }

    /// A mic that reconnected after a dropout: full length, but with a long silent fill.
    func testLongDropoutIsFlagged() {
        let issues = RecordingSession.continuityWarnings(["mic": stats("mic", seconds: 600, gaps: 42, events: 1), "system": stats("system", seconds: 600)])
        XCTAssertEqual(issues.map(\.code), ["mic_gaps"])
        XCTAssertTrue(issues[0].message.contains("0:42"), issues[0].message)
    }

    func testTrackWithNoAudioIsLeftToTheSilenceCheck() {
        var mic = stats("mic", seconds: 0)
        mic.capturedFrames = 0
        XCTAssertEqual(RecordingSession.continuityWarnings(["mic": mic, "system": stats("system", seconds: 600)]), [])
    }

    func testLiveHealth() {
        XCTAssertNil(CaptureHealth(elapsedSeconds: 3, micStalledSeconds: nil, systemStalledSeconds: nil).problem, "too early to tell")
        XCTAssertNil(CaptureHealth(elapsedSeconds: 60, micStalledSeconds: 0.1, systemStalledSeconds: 0.2).problem)
        let mic = CaptureHealth(elapsedSeconds: 60, micStalledSeconds: 12, systemStalledSeconds: 0.1).problem
        XCTAssertTrue(mic?.contains("microphone") == true, mic ?? "nil")
        let never = CaptureHealth(elapsedSeconds: 8, micStalledSeconds: nil, systemStalledSeconds: 0.1).problem
        XCTAssertTrue(never?.contains("nothing for 8 s") == true, never ?? "nil")
        let system = CaptureHealth(elapsedSeconds: 60, micStalledSeconds: 0.1, systemStalledSeconds: 30).problem
        XCTAssertTrue(system?.contains("call's audio") == true, system ?? "nil")
        XCTAssertNil(CaptureHealth(elapsedSeconds: 60, micStalledSeconds: 0.1, systemStalledSeconds: nil).problem,
                     "call audio that hasn't started yet isn't a fault")
    }

    func testClock() {
        XCTAssertEqual(MicCapture.clock(25.2), "0:25")
        XCTAssertEqual(MicCapture.clock(2506.6), "41:46")
        XCTAssertEqual(MicCapture.clock(3723), "1:02:03")
    }
}
