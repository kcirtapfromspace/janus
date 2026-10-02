import Foundation
@testable import ICRecorderCore
import XCTest

final class TrackWriterTests: XCTestCase {
    private var dir: URL!

    override func setUpWithError() throws {
        dir = FileManager.default.temporaryDirectory.appendingPathComponent("trackwriter-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: dir)
    }

    private func makeWriter(rate: Double = 1000) throws -> TrackWriter {
        try TrackWriter(url: dir.appendingPathComponent("t.wav"), sampleRate: rate, gapToleranceSeconds: 0.05)
    }

    private func append(_ writer: TrackWriter, _ samples: [Float], at start: Double?) {
        samples.withUnsafeBufferPointer { writer.append($0, startSeconds: start) }
    }

    /// Returns (sampleRate, dataBytes field, samples) parsed from the written file.
    private func readWav(_ writer: TrackWriter) throws -> (UInt32, UInt32, [Int16]) {
        let data = try Data(contentsOf: writer.url)
        XCTAssertEqual(String(data: data[0..<4], encoding: .ascii), "RIFF")
        XCTAssertEqual(String(data: data[8..<12], encoding: .ascii), "WAVE")
        func u32(_ offset: Int) -> UInt32 { data[offset..<offset + 4].withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) } }
        XCTAssertEqual(u32(4), UInt32(data.count - 8), "RIFF size")
        let samples = data[44...].withUnsafeBytes { Array($0.bindMemory(to: Int16.self)) }
        return (u32(24), u32(40), samples)
    }

    func testContiguousSamplesProduceValidHeader() throws {
        let writer = try makeWriter()
        append(writer, [0.5, -0.5, 1.0, -1.0], at: 0)
        let stats = writer.finish()
        let (rate, dataBytes, samples) = try readWav(writer)
        XCTAssertEqual(rate, 1000)
        XCTAssertEqual(dataBytes, 8)
        XCTAssertEqual(samples, [16384, -16384, 32767, -32767])
        XCTAssertEqual(stats.frames, 4)
        XCTAssertEqual(stats.leadingPadFrames, 0)
    }

    func testLeadingSilenceAlignsFirstBufferToT0() throws {
        let writer = try makeWriter()
        append(writer, [0.25, 0.25], at: 0.5)
        let stats = writer.finish()
        let (_, _, samples) = try readWav(writer)
        XCTAssertEqual(samples.count, 502)
        XCTAssertTrue(samples[0..<500].allSatisfy { $0 == 0 })
        XCTAssertEqual(samples[500], 8192)
        XCTAssertEqual(stats.leadingPadFrames, 500)
        XCTAssertEqual(stats.capturedFrames, 2)
        XCTAssertEqual(stats.startOffsetSeconds, 0.5)
    }

    func testDeliveryGapIsFilledWithSilence() throws {
        let writer = try makeWriter()
        append(writer, [Float](repeating: 0.1, count: 100), at: 0)
        append(writer, [Float](repeating: 0.1, count: 100), at: 0.3)  // expected at 0.1 s: 200 ms gap
        let stats = writer.finish()
        XCTAssertEqual(stats.gapFillEvents, 1)
        XCTAssertEqual(stats.gapFillFrames, 200)
        XCTAssertEqual(stats.frames, 400)
    }

    func testJitterWithinToleranceIsNotFilled() throws {
        let writer = try makeWriter()
        append(writer, [Float](repeating: 0.1, count: 100), at: 0)
        append(writer, [Float](repeating: 0.1, count: 100), at: 0.13)  // 30 ms late, under 50 ms tolerance
        append(writer, [Float](repeating: 0.1, count: 100), at: 0.19)  // 10 ms early relative to file
        let stats = writer.finish()
        XCTAssertEqual(stats.gapFillEvents, 0)
        XCTAssertEqual(stats.frames, 300)
        XCTAssertEqual(stats.maxAheadSeconds, 0.01, accuracy: 1e-9)
    }

    func testAudioBeforeT0IsDropped() throws {
        let writer = try makeWriter()
        append(writer, [0.1, 0.2, 0.3, 0.4], at: -0.002)
        let stats = writer.finish()
        let (_, _, samples) = try readWav(writer)
        XCTAssertEqual(stats.droppedLeadingFrames, 2)
        XCTAssertEqual(samples.count, 2)
        XCTAssertEqual(samples[0], Int16((0.3 * 32767 as Float).rounded()))
    }

    func testStatsIgnoreInsertedSilence() throws {
        let writer = try makeWriter()
        append(writer, [0.5, -0.5], at: 1.0)
        let stats = writer.finish()
        XCTAssertEqual(stats.rms, 0.5, accuracy: 1e-6)
        XCTAssertEqual(stats.peak, 0.5, accuracy: 1e-6)
    }

    func testAllZeroInputReportsZeroPeak() throws {
        let writer = try makeWriter()
        append(writer, [Float](repeating: 0, count: 1000), at: 0)
        let stats = writer.finish()
        XCTAssertEqual(stats.peak, 0)
        XCTAssertEqual(stats.capturedFrames, 1000)
    }

    func testFinishIsIdempotentAndLaterAppendsAreIgnored() throws {
        let writer = try makeWriter()
        append(writer, [0.1], at: 0)
        let first = writer.finish()
        append(writer, [0.1, 0.1], at: 0.001)
        XCTAssertEqual(writer.finish(), first)
        XCTAssertEqual(try readWav(writer).2.count, 1)
    }

    func testHeaderIsKeptCurrentDuringLongRecordings() throws {
        let writer = try makeWriter()
        append(writer, [Float](repeating: 0.1, count: 1500), at: 0)  // crosses the 1 s header refresh
        // Not finished: simulates a crash. The header should already describe the data.
        let data = try Data(contentsOf: writer.url)
        let dataBytes = data[40..<44].withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
        XCTAssertEqual(dataBytes, 3000)
        writer.finish()
    }

    func testNonFiniteSamplesBecomeSilence() throws {
        let writer = try makeWriter()
        append(writer, [.nan, .infinity, 0.5], at: 0)
        writer.finish()
        XCTAssertEqual(try readWav(writer).2, [0, 0, 16384])
    }
}
