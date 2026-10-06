import AVFoundation
import CoreAudio
import Foundation
import Darwin

public struct RecorderOptions {
    public var sessionDir: URL
    public var duration: Double?
    public var aec: Bool
    /// Also record the chosen video source to video.mov.
    public var video: Bool
    public var videoSource: VideoSource
    /// Show the Screen Recording prompt when starting without permission. Off in the app, which
    /// asks before recording rather than as the interview begins.
    public var askForScreenPermission: Bool

    public init(sessionDir: URL, duration: Double?, aec: Bool, video: Bool = false,
                askForScreenPermission: Bool = true, videoSource: VideoSource = .automatic) {
        self.sessionDir = sessionDir
        self.duration = duration
        self.aec = aec
        self.video = video
        self.videoSource = videoSource
        self.askForScreenPermission = askForScreenPermission
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
    public var alignmentNote = "Both WAVs start at t0 (the session start host time). Leading silence and delivery gaps are filled with zeros, so sample N of each file is the same instant; no offsets need to be applied. video.mov, when present, also starts at t0 (a black frame until the call window appears)."
    public var sessionDir: String
    public var startedAt: String?
    public var stoppedAt: String
    public var stopReason: String
    public var requestedDurationSeconds: Double?
    public var t0HostTime: UInt64?
    public var tracks: [String: TrackWriter.Stats]
    /// The call's video, when it was requested and something was recorded. It starts at t0 too.
    public var video: ScreenCapture.Stats?
    public var warnings: [Issue]
    public var errors: [Issue]
}

/// Whether both tracks are receiving audio right now, for a live warning while recording.
public struct CaptureHealth: Equatable {
    public var elapsedSeconds: Double
    /// Seconds since each track last received audio (nil: never has).
    public var micStalledSeconds: Double?
    public var systemStalledSeconds: Double?
    public var videoProblem: String?

    public init(elapsedSeconds: Double, micStalledSeconds: Double?, systemStalledSeconds: Double?, videoProblem: String? = nil) {
        self.elapsedSeconds = elapsedSeconds
        self.micStalledSeconds = micStalledSeconds
        self.systemStalledSeconds = systemStalledSeconds
        self.videoProblem = videoProblem
    }

    /// What to tell the person recording, or nil when both tracks are flowing. Both sources
    /// deliver buffers continuously (zeros when quiet), so a few seconds with none means capture
    /// has stopped, not that nobody is talking. The mic must start flowing on its own; call audio
    /// is only flagged once it has flowed and then stopped, in case a tap waits for playback.
    public var problem: String? {
        let threshold = 5.0
        guard elapsedSeconds > threshold else { return videoProblem }
        if (micStalledSeconds ?? elapsedSeconds) > threshold {
            return "Your microphone isn't being recorded (nothing for \(Int(micStalledSeconds ?? elapsedSeconds)) s) — trying to reconnect. Check your input device in System Settings › Sound."
        }
        if let stalled = systemStalledSeconds, stalled > threshold {
            return "The call's audio isn't being recorded (nothing for \(Int(stalled)) s)."
        }
        return videoProblem
    }
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
    private var screen: ScreenCapture?
    private var systemWriter: TrackWriter?
    private var micWriter: TrackWriter?
    private var t0HostTime: UInt64?
    private var startedAt: Date?
    private var warnings: [Issue] = []
    private var errors: [Issue] = []
    private var ownsPidFile = false
    private var finished = false
    private var startRequested = false
    private var directoryLock: Int32 = -1
    private let requestMicrophoneAccess: (@escaping (Bool) -> Void) -> Void

    private var pidURL: URL { options.sessionDir.appendingPathComponent("recorder.pid") }
    private var reportURL: URL { options.sessionDir.appendingPathComponent("recorder.json") }
    private var systemURL: URL { options.sessionDir.appendingPathComponent("system.wav") }
    private var micURL: URL { options.sessionDir.appendingPathComponent("mic.wav") }
    private var videoURL: URL { options.sessionDir.appendingPathComponent("video.mov") }

    public convenience init(options: RecorderOptions, log: Logger, onExit: @escaping (Int32) -> Void) {
        self.init(options: options, log: log, onExit: onExit, requestMicrophoneAccess: MicCapture.requestAccess)
    }

    init(options: RecorderOptions, log: Logger, onExit: @escaping (Int32) -> Void,
         requestMicrophoneAccess: @escaping (@escaping (Bool) -> Void) -> Void) {
        self.options = options
        self.log = log
        self.onExit = onExit
        self.requestMicrophoneAccess = requestMicrophoneAccess
        tap = SystemAudioTap(log: log)
        mic = MicCapture(log: log)
    }

    deinit {
        if directoryLock >= 0 { close(directoryLock) }
    }

    public func start() {
        guard !startRequested, !finished else { return }
        startRequested = true
        log.info("ICRecorder \(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev") pid \(getpid()): session \(options.sessionDir.path), duration \(options.duration.map { "\($0)s" } ?? "until stopped"), aec \(options.aec), video \(options.video)")
        do {
            try claimDirectory()
        } catch {
            // Don't write recorder.json: the directory may belong to another recording.
            log.error("\(error)")
            releaseDirectory()
            finished = true
            onExit(2)
            return
        }
        requestMicrophoneAccess { [weak self] granted in
            guard let self, !self.finished else { return }
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

    /// Live capture state; nil before capture starts and after it stops.
    public func health() -> CaptureHealth? {
        guard let startedAt, !finished else { return nil }
        return CaptureHealth(
            elapsedSeconds: Date().timeIntervalSince(startedAt),
            micStalledSeconds: micWriter?.secondsSinceLastAudio,
            systemStalledSeconds: systemWriter?.secondsSinceLastAudio,
            videoProblem: screen?.problem
        )
    }

    // MARK: - Lifecycle

    private func claimDirectory() throws {
        let fm = FileManager.default
        // Keep the lock file's inode stable. Removing it would let another process lock a new
        // inode while the previous recorder still owns the old one.
        let lockURL = options.sessionDir.appendingPathComponent(".recorder.lock")
        directoryLock = open(lockURL.path, O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW, S_IRUSR | S_IWUSR)
        guard directoryLock >= 0 else {
            throw CaptureError.message("could not lock the recording folder")
        }
        guard flock(directoryLock, LOCK_EX | LOCK_NB) == 0 else {
            throw CaptureError.message("another recorder already owns this recording folder")
        }
        if let pidText = try? String(contentsOf: pidURL, encoding: .utf8),
           let pid = pid_t(pidText.trimmingCharacters(in: .whitespacesAndNewlines)),
           kill(pid, 0) == 0 {
            throw CaptureError.message("another recorder (pid \(pid)) is already recording into \(options.sessionDir.path)")
        }
        for url in [systemURL, micURL, videoURL] where fm.fileExists(atPath: url.path) {
            throw CaptureError.message("\(url.path) already exists; refusing to overwrite an earlier recording")
        }
        try "\(getpid())\n".write(to: pidURL, atomically: true, encoding: .utf8)
        ownsPidFile = true
    }

    private func beginCapture() {
        guard !finished else { return }
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
        if options.video {
            // After the audio has started: video is extra, and its problems become warnings.
            let screen = ScreenCapture(log: log, url: videoURL, t0HostTime: t0HostTime ?? AudioGetCurrentHostTime(),
                                       source: options.videoSource)
            self.screen = screen
            screen.start(askForPermission: options.askForScreenPermission)
        }

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
        guard ownsPidFile else {
            releaseDirectory()
            onExit(exitCode)
            return
        }
        var finalExitCode = exitCode
        log.info("stopping (\(reason))")
        tap.stop()
        mic.stop()
        let video = screen?.stop()
        warnings += screen?.warnings ?? []

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
        warnings += Self.continuityWarnings(tracks)
        warnings += mic.warnings.map { Issue(code: "mic_warning", message: $0) }
        errors += mic.errors.map { Issue(code: "mic_error", message: $0) }
        for (name, stats) in tracks where stats.writeError != nil {
            errors.append(Issue(code: "\(name)_write_error", message: stats.writeError!))
        }

        if !errors.isEmpty { finalExitCode = max(finalExitCode, 1) }
        let timestamp = ISO8601DateFormatter()
        let report = RecorderReport(
            sessionDir: options.sessionDir.path,
            startedAt: startedAt.map { timestamp.string(from: $0) },
            stoppedAt: timestamp.string(from: Date()),
            stopReason: reason,
            requestedDurationSeconds: options.duration,
            t0HostTime: t0HostTime,
            tracks: tracks,
            video: video,
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
            finalExitCode = 2
        }
        releaseDirectory()

        for (name, stats) in tracks.sorted(by: { $0.key < $1.key }) {
            log.info("\(name): \(String(format: "%.1f", stats.durationSeconds)) s, rms \(String(format: "%.5f", stats.rms)), peak \(String(format: "%.4f", stats.peak)), gap fills \(stats.gapFillEvents)")
        }
        for issue in warnings { log.warn("\(issue.code): \(issue.message)") }
        log.info("done (exit \(finalExitCode))")
        onExit(finalExitCode)
    }

    private func releaseDirectory() {
        if ownsPidFile {
            try? FileManager.default.removeItem(at: pidURL)
            ownsPidFile = false
        }
        if directoryLock >= 0 {
            close(directoryLock)
            directoryLock = -1
        }
    }

    /// Flags a track that stopped well before the other one, or that dropped out for a while.
    /// Either means part of the conversation is missing from it — on the mic track, the
    /// candidate's own answers — so the analysis must not treat the missing part as silence.
    static func continuityWarnings(_ tracks: [String: TrackWriter.Stats], tolerance: Double = 10) -> [Issue] {
        let end = tracks.values.map(\.durationSeconds).max() ?? 0
        var issues: [Issue] = []
        for (name, stats) in tracks.sorted(by: { $0.key < $1.key }) where stats.capturedFrames > 0 {
            let label = name == "mic" ? "microphone (your voice)" : "call audio (the other side)"
            let missing = end - stats.durationSeconds
            if missing > tolerance {
                issues.append(Issue(
                    code: "\(name)_stopped",
                    message: "The \(label) stopped recording at \(MicCapture.clock(stats.durationSeconds)), \(MicCapture.clock(missing)) before the end. Nothing after that was captured on this track."
                ))
            }
            let gaps = Double(stats.gapFillFrames) / stats.sampleRate
            if gaps > tolerance / 2 {
                issues.append(Issue(
                    code: "\(name)_gaps",
                    message: "The \(label) dropped out for \(MicCapture.clock(gaps)) in total (\(stats.gapFillEvents) gap(s), filled with silence). Anything said then is missing from this track."
                ))
            }
        }
        return issues
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
