@testable import ICRecorderCore
import XCTest

final class RecordingLifecycleTests: XCTestCase {
    private func directory() throws -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }

    private func session(_ dir: URL, exit: @escaping (Int32) -> Void,
                         permission: @escaping (@escaping (Bool) -> Void) -> Void) -> RecordingSession {
        RecordingSession(options: RecorderOptions(sessionDir: dir, duration: nil, aec: false),
                         log: Logger(fileURL: nil), onExit: exit, requestMicrophoneAccess: permission)
    }

    func testLatePermissionCannotRestartStoppedRecording() throws {
        let dir = try directory()
        var callback: ((Bool) -> Void)?
        var exits: [Int32] = []
        let recorder = session(dir, exit: { exits.append($0) }, permission: { callback = $0 })
        recorder.start()
        recorder.stop(reason: "cancelled while awaiting permission")
        let report = try Data(contentsOf: dir.appendingPathComponent("recorder.json"))
        callback?(true)
        callback?(false)
        recorder.start()
        recorder.stop(reason: "again")
        XCTAssertEqual(exits, [0])
        XCTAssertEqual(try Data(contentsOf: dir.appendingPathComponent("recorder.json")), report)
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent("system.wav").path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent("recorder.pid").path))
        XCTAssertNil(recorder.health())
    }

    func testRepeatedStartRequestsPermissionOnlyOnce() throws {
        let dir = try directory()
        var requests = 0
        let recorder = session(dir, exit: { _ in }, permission: { _ in requests += 1 })
        recorder.start()
        recorder.start()
        XCTAssertEqual(requests, 1)
        recorder.stop(reason: "test")
    }

    func testExclusiveOwnershipAndRelease() throws {
        let dir = try directory()
        var secondExits: [Int32] = []
        var secondRequestedPermission = false
        let first = session(dir, exit: { _ in }, permission: { _ in })
        let second = session(dir, exit: { secondExits.append($0) },
                             permission: { _ in secondRequestedPermission = true })
        first.start()
        second.start()
        second.stop(reason: "failed claimant")
        XCTAssertEqual(secondExits, [2])
        XCTAssertFalse(secondRequestedPermission)
        XCTAssertTrue(FileManager.default.fileExists(atPath: dir.appendingPathComponent("recorder.pid").path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent("recorder.json").path))
        first.stop(reason: "test")
        var thirdRequestedPermission = false
        let third = session(dir, exit: { _ in }, permission: { _ in thirdRequestedPermission = true })
        third.start()
        XCTAssertTrue(thirdRequestedPermission, "the previous owner must release its lock")
        third.stop(reason: "test")
    }

    func testExistingAudioIsPreservedWhenClaimFails() throws {
        let dir = try directory()
        let audio = dir.appendingPathComponent("mic.wav")
        let original = Data("previous recording".utf8)
        try original.write(to: audio)
        var exits: [Int32] = []
        let recorder = session(dir, exit: { exits.append($0) }, permission: { _ in XCTFail("must not capture") })
        recorder.start()
        recorder.stop(reason: "test")
        XCTAssertEqual(exits, [2])
        XCTAssertEqual(try Data(contentsOf: audio), original)
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent("recorder.json").path))
    }

    func testReportSaveFailureReturnsFailureAndReleasesOwnership() throws {
        let dir = try directory()
        try FileManager.default.createDirectory(at: dir.appendingPathComponent("recorder.json"),
                                                withIntermediateDirectories: false)
        var exits: [Int32] = []
        let recorder = session(dir, exit: { exits.append($0) }, permission: { _ in })
        recorder.start()
        recorder.stop(reason: "test")
        XCTAssertEqual(exits, [2])
        XCTAssertFalse(FileManager.default.fileExists(atPath: dir.appendingPathComponent("recorder.pid").path))
    }

    func testPermissionDenialPersistsDiagnosticAndFails() throws {
        let dir = try directory()
        var exits: [Int32] = []
        let recorder = session(dir, exit: { exits.append($0) }, permission: { $0(false) })
        recorder.start()
        XCTAssertEqual(exits, [1])
        let data = try Data(contentsOf: dir.appendingPathComponent("recorder.json"))
        let report = try JSONSerialization.jsonObject(with: data) as! [String: Any]
        let errors = report["errors"] as! [[String: Any]]
        XCTAssertEqual(errors.first?["code"] as? String, "mic_permission_denied")
    }
}
