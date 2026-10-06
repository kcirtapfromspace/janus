@testable import InterviewCoachKit
import XCTest

final class AppVersionTests: XCTestCase {
    func testKeepsTheFullPreviewVersionOfTheRunningBundle() {
        let version = AppVersion(info: [
            "ICReleaseVersion": "0.1.0-preview.18",
            "CFBundleShortVersionString": "0.1.0",
            "CFBundleVersion": "100018",
        ])
        XCTAssertEqual(version.description, "Version 0.1.0-preview.18 (build 100018)")
    }

    func testFallsBackForOlderBundlesAndUnbundledDevelopment() {
        let older = AppVersion(info: ["ICReleaseVersion": " ", "CFBundleShortVersionString": "1.2.3", "CFBundleVersion": "123"])
        XCTAssertEqual(older.description, "Version 1.2.3 (build 123)")
        XCTAssertEqual(AppVersion(info: [:]).description, "Version Development (build unknown)")
    }
}
