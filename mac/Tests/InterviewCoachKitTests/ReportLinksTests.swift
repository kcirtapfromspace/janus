@testable import InterviewCoachKit
import XCTest

final class ReportLinksTests: XCTestCase {
    func testReadsTimestampLinks() {
        XCTAssertEqual(seekSeconds(fromFragment: "t=754.0"), 754)
        XCTAssertEqual(seekSeconds(fromFragment: "#t=754.0"), 754)
        XCTAssertEqual(seekSeconds(fromFragment: "t=754"), 754)
        XCTAssertEqual(seekSeconds(fromFragment: "t=12.25"), 12.25)
        XCTAssertEqual(seekSeconds(fromFragment: "t=0"), 0)
        XCTAssertEqual(seekSeconds(fromFragment: "t=.5"), 0.5)
        XCTAssertEqual(seekSeconds(fromFragment: "t=5."), 5)
        XCTAssertEqual(seekSeconds(fromFragment: " t=754.0 "), 754)
        XCTAssertEqual(seekSeconds(fromFragment: "#t = 754.0\t"), 754)
    }

    func testVersionLinksOpenThatVersion() {
        let page = URL(fileURLWithPath: "/Users/me/InterviewCoach/sessions/0001 x/reports/12.html")
        func sibling(_ name: String) -> URL { URL(fileURLWithPath: "/Users/me/InterviewCoach/sessions/0001 x/reports/" + name) }
        XCTAssertEqual(reportID(fromLink: sibling("15.html"), currentPage: page), 15)
        XCTAssertNil(reportID(fromLink: sibling("12.html"), currentPage: page), "this page")
        XCTAssertNil(reportID(fromLink: sibling("notes.html"), currentPage: page))
        XCTAssertNil(reportID(fromLink: sibling("15.pdf"), currentPage: page))
        XCTAssertNil(reportID(fromLink: URL(fileURLWithPath: "/tmp/15.html"), currentPage: page), "another folder")
        XCTAssertNil(reportID(fromLink: URL(string: "https://example.com/15.html")!, currentPage: page))
    }

    func testIgnoresEverythingElse() {
        for fragment in [nil, "", "#", "section-2", "t", "t=", "#t=", "x=754", "time=754", "t=abc", "t=12abc",
                         "t=-5", "t=+5", "t=1e3", "t=0x10", "t=inf", "t=nan", "t=1.2.3", "t=.", "t=10,20",
                         "t=1 2", "t=٣"] {
            XCTAssertNil(seekSeconds(fromFragment: fragment), "\(fragment ?? "nil")")
        }
    }
}
