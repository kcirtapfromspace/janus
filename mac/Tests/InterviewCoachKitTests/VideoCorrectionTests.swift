import Foundation
@testable import InterviewCoachKit
import XCTest

final class VideoCorrectionTests: XCTestCase {
    private func cues(onCamera: Double = 1, nods: Int = 0, away: Double? = 0.2) -> SessionDetail.VideoCues {
        SessionDetail.VideoCues(onCamera: onCamera, nods: nods, lookingAway: away, seenS: 30, faceH: 0.2)
    }

    private func labels(_ c: VideoCorrection) -> [String: Any] {
        (try? JSONSerialization.jsonObject(with: Data(c.labelsJSON.utf8))) as? [String: Any] ?? [:]
    }

    /// The form starts from what the video measured, so you only change what it got wrong.
    func testStartsFromWhatWasMeasured() {
        let c = VideoCorrection(measured: cues(onCamera: 2.2, nods: 4, away: 0.55))
        XCTAssertEqual(c.pick("on_camera"), "2")
        XCTAssertEqual(c.pick("nodded"), "true")
        XCTAssertEqual(c.pick("nod_count"), "3-5")
        XCTAssertEqual(c.pick("looked_away"), "mostly")
        XCTAssertEqual(c.pick("smiled"), VideoCorrection.unsure, "smiles aren't measured yet")
        XCTAssertEqual(VideoCorrection(measured: cues(away: nil)).pick("looked_away"), VideoCorrection.unsure)
    }

    /// The same consistency rules as the labelling page, so the label is never refused.
    func testAnswersStayConsistent() {
        var c = VideoCorrection(measured: cues(nods: 0))
        c.set("nod_count", "1-2")
        XCTAssertEqual(c.pick("nodded"), "true")
        c.set("nodded", "false")
        XCTAssertEqual(c.pick("nod_count"), "none")
        c.set("nodded", "true")
        XCTAssertEqual(c.pick("nod_count"), VideoCorrection.unsure, "a nod, but how many isn't known yet")
        c.set("smiled", "true")
        c.set("on_camera", "0")
        XCTAssertEqual([c.pick("nodded"), c.pick("nod_count"), c.pick("smiled")], ["false", "none", "false"])
    }

    func testTheLabelLeavesOutWhatYouCouldntTell() {
        var c = VideoCorrection(measured: cues(nods: 2, away: nil))
        c.set("on_camera", "1")
        let json = labels(c)
        XCTAssertEqual(json["nodded"] as? Bool, true)
        XCTAssertEqual(json["nod_count"] as? String, "1-2")
        XCTAssertEqual(json["on_camera"] as? String, "1")
        XCTAssertNil(json["looked_away"], "can't tell")
        XCTAssertNil(json["smiled"])
    }

    func testAnEarlierCorrectionComesBack() throws {
        let decoded = try JSONDecoder().decode([String: LabelValue].self, from: Data(#"{"nodded": false, "on_camera": "2"}"#.utf8))
        let c = VideoCorrection(corrected: decoded)
        XCTAssertEqual(c.pick("nodded"), "false")
        XCTAssertEqual(c.pick("on_camera"), "2")
        XCTAssertEqual(c.pick("looked_away"), VideoCorrection.unsure)
    }

    func testFixLinks() {
        XCTAssertEqual(fixSeconds(fromFragment: "fix=95.0"), 95.0)
        XCTAssertEqual(fixSeconds(fromFragment: "#fix=5"), 5.0)
        XCTAssertNil(fixSeconds(fromFragment: "t=95.0"), "a timestamp isn't a correction")
        XCTAssertNil(seekSeconds(fromFragment: "fix=95.0"), "and a correction isn't a timestamp")
        XCTAssertNil(fixSeconds(fromFragment: "fix=-1"))
    }

    func testSessionDetailDecodesVideoAnswers() throws {
        let json = #"""
        {"start": 5.0, "end": 30.0, "timestamp": "00:00:05", "question": "Tell me about it.",
         "cues": {"on_camera": 1.0, "nods": 3, "looking_away": 0.0, "seen_s": 25.0, "face_h": 0.25},
         "notes": ["they nodded 3 times"], "corrected": {"nodded": true, "nod_count": "1-2"}}
        """#
        let answer = try ICClient.decode(SessionDetail.VideoAnswer.self, from: Data(json.utf8))
        XCTAssertEqual(answer.cues.nods, 3)
        XCTAssertEqual(answer.corrected?["nod_count"], .string("1-2"))
    }
}
