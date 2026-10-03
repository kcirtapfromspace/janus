@testable import ICRecorderCore
import XCTest

final class AudioSelfTestTests: XCTestCase {
    private func report(micPeak: Double, micFrames: Int, systemPeak: Double, warnings: [String] = []) -> Data {
        let json: [String: Any] = [
            "tracks": [
                "mic": ["peak": micPeak, "captured_frames": micFrames],
                "system": ["peak": systemPeak, "captured_frames": 240_000],
            ],
            "warnings": warnings.map { ["code": "x", "message": $0] },
            "errors": [],
        ]
        return try! JSONSerialization.data(withJSONObject: json)
    }

    func testBothTracksRecording() {
        let result = AudioSelfTest.result(fromReport: report(micPeak: 0.02, micFrames: 240_000, systemPeak: 0.4))
        XCTAssertTrue(result.micOK && result.systemOK)
    }

    /// A denied System Audio Recording permission records silence instead of failing.
    func testSilentCallAudioMeansNoPermission() {
        let result = AudioSelfTest.result(fromReport: report(micPeak: 0.02, micFrames: 240_000, systemPeak: 0,
                                                             warnings: ["Every system sample is exactly zero."]))
        XCTAssertTrue(result.micOK)
        XCTAssertFalse(result.systemOK)
        XCTAssertEqual(result.issues, ["Every system sample is exactly zero."])
    }

    func testNoMicAudio() {
        XCTAssertFalse(AudioSelfTest.result(fromReport: report(micPeak: 0, micFrames: 0, systemPeak: 0.4)).micOK)
    }

    func testMissingReport() {
        let result = AudioSelfTest.result(fromReport: Data())
        XCTAssertFalse(result.micOK || result.systemOK)
        XCTAssertEqual(result.issues, ["The test didn't produce a result."])
    }
}
