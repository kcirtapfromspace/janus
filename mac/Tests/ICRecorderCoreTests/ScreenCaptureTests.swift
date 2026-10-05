@testable import ICRecorderCore
import XCTest

final class ScreenCaptureTests: XCTestCase {
    private func window(_ id: UInt32, _ bundle: String, _ title: String, _ w: Double = 1440, _ h: Double = 900) -> CallWindowCandidate {
        CallWindowCandidate(windowID: id, bundleID: bundle, appName: bundle, title: title, width: w, height: h)
    }

    func testTheMeetingWindowBeatsItsAppsHomeWindow() {
        let home = window(1, "us.zoom.xos", "Zoom Workplace", 1600, 1000)
        let meeting = window(2, "us.zoom.xos", "Zoom Meeting", 1280, 800)
        XCTAssertEqual(CallWindows.choose([home, meeting])?.windowID, 2)
        XCTAssertEqual(CallWindows.choose([home])?.windowID, 1, "a call app's only window is still worth recording")
    }

    func testBrowsersOnlyCountWhenTheTabIsACall() {
        let meet = window(3, "com.google.Chrome", "Meet - abc-defg-hij")
        let email = window(4, "com.google.Chrome", "Inbox (3) - Gmail", 1900, 1100)
        XCTAssertEqual(CallWindows.choose([email, meet])?.windowID, 3)
        XCTAssertNil(CallWindows.choose([email]), "never record an ordinary browser window")
    }

    /// A tab's title is somebody's document unless a call service wrote it.
    func testBrowserTabsThatOnlyMentionCallsAreNotCalls() {
        for title in ["Interview notes - Google Docs", "#oncall | Slack", "Callback handling - Confluence",
                      "Meeting notes - Notion", "Zoom pricing - Google Search", "Chat | Microsoft Teams"] {
            XCTAssertNil(CallWindows.choose([window(7, "com.google.Chrome", title)]), title)
        }
        for title in ["Meet – Weekly sync", "Zoom Meeting", "Meeting with Sam | Microsoft Teams", "Call with Sam | Microsoft Teams"] {
            XCTAssertEqual(CallWindows.choose([window(8, "com.apple.Safari", title)])?.windowID, 8, title)
        }
    }

    func testCallWordsAreWholeWords() {
        XCTAssertEqual(CallWindows.tier(window(9, "com.tinyspeck.slackmacgap", "#oncall - Acme")), 1, "not a huddle")
        XCTAssertEqual(CallWindows.tier(window(9, "com.tinyspeck.slackmacgap", "Huddle in #design")), 2)
    }

    func testOtherAppsAndTinyWindowsAreNeverRecorded() {
        XCTAssertNil(CallWindows.choose([window(5, "com.apple.Notes", "Interview notes")]))
        XCTAssertNil(CallWindows.choose([window(6, "us.zoom.xos", "Zoom Meeting", 320, 180)]), "the floating mini-player")
    }

    func testSwitchingOnlyWhenTheWindowIsGoneOrAClearerCallAppears() {
        let home = window(1, "us.zoom.xos", "Zoom Workplace")
        let meeting = window(2, "us.zoom.xos", "Zoom Meeting")
        let other = window(3, "com.google.Chrome", "Meet - xyz")
        XCTAssertTrue(CallWindows.shouldSwitch(from: nil, to: meeting, available: [meeting]))
        XCTAssertTrue(CallWindows.shouldSwitch(from: home, to: meeting, available: [home, meeting]), "the meeting opened")
        XCTAssertFalse(CallWindows.shouldSwitch(from: meeting, to: other, available: [meeting, other]), "equally clear: stay put")
        XCTAssertTrue(CallWindows.shouldSwitch(from: meeting, to: other, available: [other]), "the meeting window closed")
        XCTAssertFalse(CallWindows.shouldSwitch(from: meeting, to: nil, available: []))
    }

    /// The tab's title changes when you look at another tab; the window being recorded stays.
    func testARetitledWindowIsKept() {
        let meet = window(3, "com.google.Chrome", "Meet - xyz")
        var retitled = meet
        retitled.title = "Inbox - Gmail"
        let zoom = window(1, "us.zoom.xos", "Zoom Workplace")
        XCTAssertFalse(CallWindows.shouldSwitch(from: meet, to: zoom, available: [retitled, zoom]))
    }

    func testVideoTimesStartAtT0AndOnlyMoveForward() {
        var clock = VideoClock()
        XCTAssertNil(clock.time(forSecondsSinceT0: -0.2), "frames from before t0 are dropped")
        XCTAssertEqual(clock.time(forSecondsSinceT0: 0)?.seconds, 0)
        XCTAssertEqual(clock.time(forSecondsSinceT0: 0.1)?.seconds ?? 0, 0.1, accuracy: 1e-9)
        XCTAssertNil(clock.time(forSecondsSinceT0: 0.1), "a repeated time")
        XCTAssertNil(clock.time(forSecondsSinceT0: 0.05), "an earlier time")
        XCTAssertEqual(clock.time(forSecondsSinceT0: 125.4)?.seconds ?? 0, 125.4, accuracy: 1e-9, "a window that appears late keeps its real time")
    }

    /// The reviewer's case: frames under half a 1/600 s tick apart would round to the same time and fail the writer.
    func testFramesInsideOneTickAreDropped() {
        var clock = VideoClock()
        XCTAssertNotNil(clock.time(forSecondsSinceT0: 1.0))
        XCTAssertNil(clock.time(forSecondsSinceT0: 1.0005), "0.5 ms later: the same tick once rounded")
        XCTAssertNotNil(clock.time(forSecondsSinceT0: 1.0034), "the next tick")
    }
}
