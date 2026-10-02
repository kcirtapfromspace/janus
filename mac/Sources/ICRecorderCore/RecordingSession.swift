import AVFoundation
import CoreAudio
import Foundation

public struct RecorderOptions {
    public var sessionDir: URL
    public var duration: Double?
    public var aec: Bool

    public init(sessionDir: URL, duration: Double?, aec: Bool) {
        self.sessionDir = sessionDir
        self.duration = duration
        self.aec = aec
    }
}

/// A warning or error in recorder.json. `code` is stable for programmatic checks.
public struct Issue: Codable, Equatable {
    public var code: String
    public var message: String
}

/// Contents of recorder.json, written when recording stops (or fails to start).
public struct RecorderReport: Codable {
    public var version = 1
    public var alignment = "t0_padded"
    public var alignmentNote = "Both WAVs start at t0 (the session start host time). Leading silence and delivery gaps are filled with zeros, so sample N of each file is the same instant; no offsets need to be applied."
    public var sessionDir: String
    public var startedAt: String?
    public var stoppedAt: String
    public var stopReason: String
    public var requestedDurationSeconds: Double?
    public var t0HostTime: UInt64?
    public var tracks: [String: TrackWriter.Stats]
    public var warnings: [Issue]
    public var errors: [Issue]
}

/// Owns one recording: claims the session directory, starts both tracks against a shared t0,
/// and on stop finalizes the WAVs, writes recorder.json and removes the pid file.
/// All methods run on the main queue.
public final class RecordingSession {
    public let options: RecorderOptions
    private let log: Logger
    private let onExit: (Int32) -> Void
    private let tap: SystemAudioTap
    private let mic: MicCapture
    private var systemWriter: TrackWriter?
    private var micWriter: TrackWriter?
    private var t0HostTime: UInt64?
    private var startedAt: Date?
    private var warnings: [Issue] = []
    private var errors: [Issue] = []
    private var ownsPidFile = false
    private var finished = false

    private var pidURL: URL { options.sessionDir.appendingPathComponent("recorder.pid") }
    private var reportURL: URL { options.sessionDir.appendingPathComponent("recorder.json") }
    private var systemURL: URL { options.sessionDir.appendingPathComponent("system.wav") }
    private var micURL: URL { options.sessionDir.appendingPathComponent("mic.wav") }

    public init(options: RecorderOptions, log: Logger, onExit: @escaping (Int32) -> Void) {
        self.options = options
        self.log = log
        self.onExit = onExit
        tap = SystemAudioTap(log: log)
        mic = MicCapture(log: log)
    }

    public func start() {
        log.info("ICRecorder \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev") pid \(getpid()): session \(options.sessionDir.path), duration \(options.duration.map { "\($0)s" } ?? "until stopped"), aec \(options.aec)")
        do {
            try claimDirectory()
        } catch {
            // Don't write recorder.json: the directory may belong to another recording.
            log.error("\(error)")
            onExit(2)
            return
        }
        MicCapture.requestAccess { [weak self] granted in
            guard let self else { return }
            guard granted else {
                self.fail("mic_permission_denied", "Microphone permission denied. Allow \(appName) in System Settings > Privacy & Security > Microphone.")
                return
            }
            self.beginCapture()
        }
    }

    public func stop(reason: String) {
        finish(reason: reason, exitCode: 0)
    }

    // MARK: - Lifecycle

    private func claimDirectory() throws {
        let fm = FileManager.default
        if let pidText = try? String(contentsOf: pidURL, encoding: .utf8),
           let pid = pid_t(pidText.trimmingCharacters(in: .whitespacesAndNewlines)),
           kill(pid, 0) == 0 {
            throw CaptureError.message("another recorder (pid \(pid)) is already recording into \(options.sessionDir.path)")
        }
        for url in [systemURL, micURL] where fm.fileExists(atPath: url.path) {
            throw CaptureError.message("\(url.path) already exists; refusing to overwrite an earlier recording")
        }
        try "\(getpid())\n".write(to: pidURL, atomically: true, encoding: .utf8)
        ownsPidFile = true
    }

    private func beginCapture() {
        do {
            let systemRate = try tap.prepare()
            let systemWriter = try TrackWriter(url: systemURL, sampleRate: systemRate)
            self.systemWriter = systemWriter

            let t0 = AudioGetCurrentHostTime()
            t0HostTime = t0
            startedAt = Date()
            try tap.start(writer: systemWriter, t0HostTime: t0)
            try mic.start(t0HostTime: t0, aec: options.aec) { [micURL] rate in
                let writer = try TrackWriter(url: micURL, sampleRate: rate)
                self.micWriter = writer
                return writer
            }
        } catch {
            fail("start_failed", "Could not start recording: \(error)")
            return
        }
        log.info("recording started")

        if let duration = options.duration {
            DispatchQueue.main.asyncAfter(deadline: .now() + duration) { [weak self] in
                self?.stop(reason: "duration")
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 10) { [weak self] in
            guard let self, !self.finished, let writer = self.systemWriter, writer.currentPeak == 0 else { return }
            self.log.warn("system: 10 s in and every sample is exactly zero. Either nothing is playing yet, or System Audio Recording permission is missing (System Settings > Privacy & Security > Screen & System Audio Recording).")
        }
    }

    private func fail(_ code: String, _ message: String) {
        log.error(message)
        errors.append(Issue(code: code, message: message))
        finish(reason: "error", exitCode: 1)
    }

    private func finish(reason: String, exitCode: Int32) {
        guard !finished else { return }
        finished = true
        log.info("stopping (\(reason))")
        tap.stop()
        mic.stop()

        var tracks: [String: TrackWriter.Stats] = [:]
        if var stats = systemWriter?.finish() {
            stats.details = tap.details
            tracks["system"] = stats
            warnings += silenceWarnings(track: "system", stats: stats)
        }
        if var stats = micWriter?.finish() {
            stats.details = mic.details
            tracks["mic"] = stats
            warnings += silenceWarnings(track: "mic", stats: stats)
        }
        warnings += mic.warnings.map { Issue(code: "mic_warning", message: $0) }
        errors += mic.errors.map { Issue(code: "mic_error", message: $0) }
        for (name, stats) in tracks where stats.writeError != nil {
            errors.append(Issue(code: "\(name)_write_error", message: stats.writeError!))
        }

        let timestamp = ISO8601DateFormatter()
        let report = RecorderReport(
            sessionDir: options.sessionDir.path,
            startedAt: startedAt.map { timestamp.string(from: $0) },
            stoppedAt: timestamp.string(from: Date()),
            stopReason: reason,
            requestedDurationSeconds: options.duration,
            t0HostTime: t0HostTime,
            tracks: tracks,
            warnings: warnings,
            errors: errors
        )
        do {
            let encoder = JSONEncoder()
            encoder.keyEncodingStrategy = .convertToSnakeCase
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(report).write(to: reportURL, options: .atomic)
        } catch {
            log.error("could not write recorder.json: \(error)")
        }
        if ownsPidFile { try? FileManager.default.removeItem(at: pidURL) }

        for (name, stats) in tracks.sorted(by: { $0.key < $1.key }) {
            log.info("\(name): \(String(format: "%.1f", stats.durationSeconds)) s, rms \(String(format: "%.5f", stats.rms)), peak \(String(format: "%.4f", stats.peak)), gap fills \(stats.gapFillEvents)")
        }
        for issue in warnings { log.warn("\(issue.code): \(issue.message)") }
        log.info("done (exit \(exitCode))")
        onExit(exitCode)
    }

    /// Flags tracks that are silent. An all-zero system track is the signature of missing
    /// System Audio Recording permission (macOS returns zeros instead of an error).
    private func silenceWarnings(track: String, stats: TrackWriter.Stats) -> [Issue] {
        let hint = track == "system"
            ? "Nothing played, or System Audio Recording permission is missing for \(appName)."
            : "The mic may be muted, disconnected, or blocked by permissions."
        if stats.capturedFrames == 0 {
            return [Issue(code: "\(track)_no_audio", message: "No \(track) audio buffers were delivered. \(hint)")]
        }
        if stats.peak == 0 {
            return [Issue(code: "\(track)_silent", message: "Every \(track) sample is exactly zero. \(hint)")]
        }
        if stats.rms < 1e-4 {
            return [Issue(code: "\(track)_near_silent", message: "The \(track) track is nearly silent (rms \(stats.rms)). \(hint)")]
        }
        return []
    }
}
