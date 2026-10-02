import Foundation

/// Writes a mono 16-bit PCM WAV whose sample 0 is the session start (t0).
///
/// Every track in a session is written against the same t0, so the files line up
/// sample-for-sample with no offset bookkeeping downstream:
/// - the first buffer is preceded by silence covering the time from t0 to its first sample;
/// - a buffer that arrives after a delivery gap (dropped or late buffers) is preceded by
///   silence, so the file position keeps tracking the host clock instead of drifting.
///
/// The WAV header is rewritten about once a second, so a crash leaves a playable file.
/// Thread-safe: capture callbacks append while the main thread calls `finish()`.
public final class TrackWriter {
    public struct Stats: Codable, Equatable {
        public var file: String
        public var sampleRate: Double
        public var channels: Int
        /// Frames in the file, including inserted silence.
        public var frames: Int64
        public var durationSeconds: Double
        /// Frames that came from the capture device (excludes inserted silence).
        public var capturedFrames: Int64
        public var firstBufferHostTime: UInt64?
        /// Time of the first captured sample relative to t0.
        public var startOffsetSeconds: Double?
        public var leadingPadFrames: Int64
        public var droppedLeadingFrames: Int64
        public var gapFillFrames: Int64
        public var gapFillEvents: Int
        /// Largest amount the file ran ahead of the host clock (never corrected: no audio is dropped).
        public var maxAheadSeconds: Double
        /// RMS and peak of captured samples only, as linear amplitude in 0...1.
        public var rms: Double
        public var peak: Double
        public var writeError: String?
        public var details: [String: String] = [:]
    }

    public let url: URL
    public let sampleRate: Double
    private let gapToleranceFrames: Int64
    private let lock = NSLock()
    private var handle: FileHandle?
    private var framesWritten: Int64 = 0
    private var capturedFrames: Int64 = 0
    private var sumSquares: Double = 0
    private var peakValue: Float = 0
    private var started = false
    private var firstHostTime: UInt64?
    private var startOffset: Double?
    private var leadingPad: Int64 = 0
    private var droppedLeading: Int64 = 0
    private var gapFill: Int64 = 0
    private var gapEvents = 0
    private var maxAhead: Int64 = 0
    private var framesAtLastHeaderUpdate: Int64 = 0
    private var writeError: String?
    private var finishedStats: Stats?
    private var pcmScratch: [Int16] = []

    public init(url: URL, sampleRate: Double, gapToleranceSeconds: Double = 0.05) throws {
        guard sampleRate > 0 else { throw CaptureError.message("invalid sample rate \(sampleRate) for \(url.lastPathComponent)") }
        self.url = url
        self.sampleRate = sampleRate
        gapToleranceFrames = Int64((gapToleranceSeconds * sampleRate).rounded())
        guard FileManager.default.createFile(atPath: url.path, contents: Self.header(sampleRate: sampleRate, dataBytes: 0)) else {
            throw CaptureError.message("could not create \(url.path)")
        }
        let handle = try FileHandle(forWritingTo: url)
        try handle.seekToEnd()
        self.handle = handle
    }

    /// Peak of captured samples so far; used for the early "is anything arriving?" check.
    public var currentPeak: Float {
        lock.lock()
        defer { lock.unlock() }
        return peakValue
    }

    /// Appends mono samples.
    /// - Parameters:
    ///   - startSeconds: time of `samples[0]` relative to t0, or nil to append contiguously.
    ///   - hostTime: host time of `samples[0]`, recorded for the first buffer only.
    public func append(_ samples: UnsafeBufferPointer<Float>, startSeconds: Double?, hostTime: UInt64? = nil) {
        lock.lock()
        defer { lock.unlock() }
        guard handle != nil, writeError == nil, !samples.isEmpty else { return }

        var body = samples
        if let start = startSeconds {
            let expected = Int64((start * sampleRate).rounded())
            if !started {
                startOffset = start
                firstHostTime = hostTime
                if expected > 0 {
                    writeSilence(expected)
                    leadingPad = expected
                } else if expected < 0 {
                    // Audio from before t0: drop it so sample 0 stays t0.
                    let drop = Int(min(-expected, Int64(body.count)))
                    droppedLeading += Int64(drop)
                    body = UnsafeBufferPointer(rebasing: body[drop...])
                    if body.isEmpty { return }
                }
            } else {
                let delta = expected - framesWritten
                if delta > gapToleranceFrames {
                    writeSilence(delta)
                    gapFill += delta
                    gapEvents += 1
                } else if delta < 0 {
                    maxAhead = max(maxAhead, -delta)
                }
            }
        } else if !started {
            firstHostTime = hostTime
        }
        started = true
        writeSamples(body)

        if framesWritten - framesAtLastHeaderUpdate >= Int64(sampleRate) {
            updateHeader()
            framesAtLastHeaderUpdate = framesWritten
        }
    }

    /// Finalizes the header and closes the file. Later appends are ignored. Idempotent.
    @discardableResult
    public func finish() -> Stats {
        lock.lock()
        defer { lock.unlock() }
        if let stats = finishedStats { return stats }
        updateHeader()
        do {
            try handle?.synchronize()
            try handle?.close()
        } catch {
            if writeError == nil { writeError = "\(error)" }
        }
        handle = nil
        let stats = Stats(
            file: url.lastPathComponent,
            sampleRate: sampleRate,
            channels: 1,
            frames: framesWritten,
            durationSeconds: Double(framesWritten) / sampleRate,
            capturedFrames: capturedFrames,
            firstBufferHostTime: firstHostTime,
            startOffsetSeconds: startOffset,
            leadingPadFrames: leadingPad,
            droppedLeadingFrames: droppedLeading,
            gapFillFrames: gapFill,
            gapFillEvents: gapEvents,
            maxAheadSeconds: Double(maxAhead) / sampleRate,
            rms: capturedFrames > 0 ? (sumSquares / Double(capturedFrames)).squareRoot() : 0,
            peak: Double(peakValue),
            writeError: writeError
        )
        finishedStats = stats
        return stats
    }

    // MARK: - Writing (lock held)

    private func writeSamples(_ samples: UnsafeBufferPointer<Float>) {
        let count = samples.count
        if pcmScratch.count < count { pcmScratch = [Int16](repeating: 0, count: count) }
        var squares = 0.0
        var peak = peakValue
        for index in 0..<count {
            var sample = samples[index]
            if !sample.isFinite { sample = 0 }
            let magnitude = abs(sample)
            if magnitude > peak { peak = magnitude }
            squares += Double(sample) * Double(sample)
            let clamped = max(-1, min(1, sample))
            pcmScratch[index] = Int16((clamped * 32767).rounded()).littleEndian
        }
        sumSquares += squares
        peakValue = peak
        capturedFrames += Int64(count)
        pcmScratch.withUnsafeBytes { raw in
            write(Data(bytes: raw.baseAddress!, count: count * MemoryLayout<Int16>.size))
        }
        framesWritten += Int64(count)
    }

    private func writeSilence(_ frames: Int64) {
        guard frames > 0 else { return }
        let chunkFrames = 65_536
        let chunk = Data(count: chunkFrames * MemoryLayout<Int16>.size)
        var remaining = frames
        while remaining > 0, writeError == nil {
            let n = Int(min(remaining, Int64(chunkFrames)))
            write(n == chunkFrames ? chunk : chunk.prefix(n * MemoryLayout<Int16>.size))
            remaining -= Int64(n)
        }
        framesWritten += frames
    }

    private func write(_ data: Data) {
        do {
            try handle?.write(contentsOf: data)
        } catch {
            if writeError == nil { writeError = "\(error)" }
        }
    }

    private func updateHeader() {
        guard let handle else { return }
        let dataBytes = UInt32(clamping: framesWritten * Int64(MemoryLayout<Int16>.size))
        do {
            try handle.seek(toOffset: 4)
            try handle.write(contentsOf: Self.le32(dataBytes &+ 36))
            try handle.seek(toOffset: 40)
            try handle.write(contentsOf: Self.le32(dataBytes))
            try handle.seekToEnd()
        } catch {
            if writeError == nil { writeError = "\(error)" }
        }
    }

    // MARK: - WAV header

    static func header(sampleRate: Double, dataBytes: UInt32) -> Data {
        let rate = UInt32(sampleRate.rounded())
        var data = Data()
        data.append(contentsOf: Array("RIFF".utf8))
        data.append(le32(dataBytes &+ 36))
        data.append(contentsOf: Array("WAVE".utf8))
        data.append(contentsOf: Array("fmt ".utf8))
        data.append(le32(16))           // fmt chunk size
        data.append(le16(1))            // PCM
        data.append(le16(1))            // mono
        data.append(le32(rate))
        data.append(le32(rate * 2))     // byte rate
        data.append(le16(2))            // block align
        data.append(le16(16))           // bits per sample
        data.append(contentsOf: Array("data".utf8))
        data.append(le32(dataBytes))
        return data
    }

    private static func le32(_ value: UInt32) -> Data { withUnsafeBytes(of: value.littleEndian) { Data($0) } }
    private static func le16(_ value: UInt16) -> Data { withUnsafeBytes(of: value.littleEndian) { Data($0) } }
}
