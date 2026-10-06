import AVFoundation
import CoreGraphics
import CoreMedia
import Foundation
import ScreenCaptureKit

/// A window a call might be in. Plain values, so choosing one is testable without a screen.
public struct CallWindowCandidate: Equatable {
    public var windowID: UInt32
    public var bundleID: String
    public var appName: String
    public var title: String
    public var width: Double
    public var height: Double

    public init(windowID: UInt32, bundleID: String, appName: String, title: String, width: Double, height: Double) {
        self.windowID = windowID
        self.bundleID = bundleID
        self.appName = appName
        self.title = title
        self.width = width
        self.height = height
    }

    public var label: String { title.isEmpty ? appName : "\(appName): \(title)" }
}

/// Heuristics for automatic mode. Explicitly picked sources bypass these rules.
public enum CallWindows {
    /// Apps that are only ever used for calls (any of their windows may be the call).
    static let callApps: Set<String> = [
        "us.zoom.xos", "com.microsoft.teams2", "com.microsoft.teams", "Cisco-Systems.Spark",
        "com.cisco.webexmeetingsapp", "com.webex.meetingmanager", "com.apple.FaceTime",
        "com.tinyspeck.slackmacgap", "com.hnc.Discord", "co.around.Around", "com.ringcentral.glip",
    ]
    static let browsers: Set<String> = [
        "com.google.Chrome", "com.google.Chrome.canary", "com.apple.Safari", "company.thebrowser.Browser",
        "com.microsoft.edgemac", "org.mozilla.firefox", "com.brave.Browser", "com.vivaldi.Vivaldi",
        "com.operasoftware.Opera",
    ]
    /// Whole words that mean a call app's window is the call ("Zoom Meeting", "Call with Sam | Microsoft Teams").
    static let callTitleWords: Set<String> = ["meeting", "webinar", "huddle", "call", "interview"]
    /// A browser window's title is its open tab's, so only a call service's own title counts there:
    /// a tab called "Interview notes" or "#oncall" is somebody's document, not the call.
    static let browserCallTitles = ["meet -", "meet –", "meet.google.com", "zoom meeting", "zoom webinar", "whereby", "jitsi meet"]
    /// Services whose tabs are a call only when the title also says so ("Meeting with Sam | Microsoft Teams").
    static let browserCallServices = ["microsoft teams", "webex"]

    static func words(_ title: String) -> Set<String> {
        Set(title.lowercased().split { !$0.isLetter }.map(String.init))
    }
    /// Smaller windows are a call app's floating mini-player or a toolbar, not the call.
    static let minWidth = 400.0, minHeight = 300.0

    /// 2: a window whose title says it's a call; 1: another window of a call app; 0: not a call.
    static func tier(_ w: CallWindowCandidate) -> Int {
        guard w.width >= minWidth, w.height >= minHeight else { return 0 }
        let title = w.title.lowercased()
        let saysCall = !words(title).isDisjoint(with: callTitleWords)
        if callApps.contains(w.bundleID) { return saysCall ? 2 : 1 }
        if browsers.contains(w.bundleID) {
            let call = browserCallTitles.contains { title.contains($0) }
                || (saysCall && browserCallServices.contains { title.contains($0) })
            return call ? 2 : 0
        }
        return 0
    }

    /// The best call window: the clearest call, then the largest.
    public static func choose(_ windows: [CallWindowCandidate]) -> CallWindowCandidate? {
        windows
            .filter { tier($0) > 0 }
            .max { (tier($0), $0.width * $0.height) < (tier($1), $1.width * $1.height) }
    }

    /// Whether to move from the window being recorded to `best`: only when the current one is gone,
    /// or a clearer call appears (the meeting window opening after its app's home window). Never
    /// between two equally good windows, so the video doesn't flick between them. `current` is the
    /// window as it was chosen: a Meet window whose title changes when you look at another tab
    /// keeps its place.
    public static func shouldSwitch(from current: CallWindowCandidate?, to best: CallWindowCandidate?, available: [CallWindowCandidate]) -> Bool {
        guard let best else { return false }
        guard let current, let live = available.first(where: { $0.windowID == current.windowID }) else { return true }
        return best.windowID != live.windowID && tier(best) > max(tier(live), tier(current))
    }
}

/// Presentation times for the video file, where 0 is t0, the instant both audio tracks start.
/// Frames from before t0, or not after the previous frame, are dropped: the writer needs strictly
/// increasing times, compared in the file's own ticks (1/600 s), since two frames under a tick
/// apart would round to the same time and fail the writer.
struct VideoClock {
    static let timescale: CMTimeScale = 600
    private(set) var last: CMTime?

    var lastSeconds: Double? { last?.seconds }

    mutating func time(forSecondsSinceT0 seconds: Double) -> CMTime? {
        guard seconds >= 0 else { return nil }
        let time = CMTime(value: CMTimeValue((seconds * Double(Self.timescale)).rounded()), timescale: Self.timescale)
        if let last, CMTimeCompare(time, last) <= 0 { return nil }
        last = time
        return time
    }
}

/// Records the call's window to a video file, time-aligned with the audio tracks: second N of
/// the video is second N of `mic.wav` and `system.wav`. The file starts with a black frame at 0,
/// so it lines up even when the call window appears after recording starts.
///
/// Needs Screen Recording permission. Video is extra: every failure (no permission, no call
/// window, an encoder error) becomes a warning and the audio carries on.
/// `start()` and `stop()` run on the main queue; frames arrive on a private queue.
public final class ScreenCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    public struct Stats: Codable, Equatable {
        public var file: String
        public var width: Int
        public var height: Int
        public var framesPerSecond: Double
        /// Frames written, including the black frame at 0.
        public var frames: Int
        public var droppedFrames: Int
        public var durationSeconds: Double
        /// When the first frame of the call arrived, relative to t0.
        public var firstFrameSeconds: Double?
        /// Each window recorded, in order ("zoom.us: Zoom Meeting").
        public var windows: [String]
        public var writeError: String?
    }

    public static let width = 1280, height = 720
    public static let framesPerSecond = 10.0
    static let rescanSeconds = 5.0

    public private(set) var warnings: [Issue] = []
    /// Shown while recording, so a missing video source is discovered before the interview ends.
    public private(set) var problem: String?

    private let log: Logger
    private let url: URL
    private let t0Seconds: Double
    private let source: VideoSource
    private let queue = DispatchQueue(label: "com.thinkstudio.interviewcoach.recorder.video", qos: .userInitiated)
    private var stream: SCStream?
    private var current: AutomaticVideoTarget?
    private var windows: [String] = []
    private var rescanTimer: Timer?
    private var stopped = false
    private var permissionChecked = false
    /// Why capturing the chosen window last failed, if it did.
    private var captureError: String?
    private var enumerationError: String?
    private var captureRevoked = false
    // Written on `queue` only.
    private var writer: AVAssetWriter?
    private var input: AVAssetWriterInput?
    private var adaptor: AVAssetWriterInputPixelBufferAdaptor?
    private var clock = VideoClock()
    private var frames = 0
    private var dropped = 0
    private var firstFrame: Double?
    private var writeError: String?
    private var queueStopped = false

    public init(log: Logger, url: URL, t0HostTime: UInt64, source: VideoSource = .automatic) {
        self.log = log
        self.url = url
        t0Seconds = AVAudioTime.seconds(forHostTime: t0HostTime)
        self.source = source
    }

    /// Whether this app may automatically capture screen content. Asking prompts once; macOS applies a new grant
    /// only after the app is reopened.
    public static func hasPermission() -> Bool { CGPreflightScreenCaptureAccess() }
    @discardableResult public static func requestPermission() -> Bool { CGRequestScreenCaptureAccess() }

    /// `askForPermission`: show the system prompt if permission is missing. The app asks from its
    /// Record window beforehand, so a prompt never appears as an interview starts; `ic record
    /// --video` has no other moment to ask.
    public func start(askForPermission: Bool) {
        // A system-picker filter grants access to that selection, even without a blanket grant.
        let picked: Bool = if case .selected = source { true } else { false }
        guard picked || Self.hasPermission() else {
            if askForPermission { Self.requestPermission() }
            warn("video_permission_denied", "Screen Recording permission is off for \(appName), so the call's video wasn't recorded (the audio was). Turn on \(appName) in System Settings > Privacy & Security > Screen & System Audio Recording, then reopen \(appName).")
            problem = "Video isn't recording: Screen Recording permission is off. Audio is recording."
            return
        }
        permissionChecked = true
        rescan()
        let timer = Timer(timeInterval: Self.rescanSeconds, repeats: true) { [weak self] _ in self?.rescan() }
        RunLoop.main.add(timer, forMode: .common)
        rescanTimer = timer
    }

    /// Stops capture and finalizes the file. Blocks until it's written (at most a few seconds).
    public func stop() -> Stats? {
        stopped = true
        rescanTimer?.invalidate()
        rescanTimer = nil
        stream?.stopCapture { _ in }
        stream = nil
        guard permissionChecked else { return nil }
        let end = AVAudioTime.seconds(forHostTime: mach_absolute_time()) - t0Seconds
        let (writer, input) = queue.sync { () -> (AVAssetWriter?, AVAssetWriterInput?) in
            queueStopped = true
            return (self.writer, self.input)
        }
        guard let writer, let input else {
            if windows.isEmpty {
                if let enumerationError {
                    warn("video_enumeration_failed", "Janus couldn't list capture sources: \(enumerationError). No video was recorded. The audio was.")
                } else {
                    warn("video_no_call_window", "Janus didn't detect a meeting window, so no video was recorded. A meeting may have been open. Next time, choose a window, app or screen in the Record window, or enable the screen fallback. The audio was recorded.")
                }
            } else {
                warn("video_capture_failed", "The selected video source (\(windows.joined(separator: ", "))) couldn't be recorded\(captureError.map { ": \($0)" } ?? ""). The audio was.")
            }
            return nil
        }
        queue.sync {
            if writer.status == .writing {
                input.markAsFinished()
                let lastFrame = clock.last ?? .zero
                let stop = CMTime(seconds: max(end, 0), preferredTimescale: VideoClock.timescale)
                writer.endSession(atSourceTime: CMTimeMaximum(stop, lastFrame))
            }
        }
        // Blocks the caller (the main queue) while the file is finalized: recorder.json must
        // describe a finished file, and ICRecorder exits right after.
        let done = DispatchSemaphore(value: 0)
        if writer.status == .writing {
            writer.finishWriting { done.signal() }
            if done.wait(timeout: .now() + 20) == .timedOut {
                writeError = "timed out finishing the video file"
            }
        }
        if writer.status == .failed {
            writeError = writer.error.map { "\($0.localizedDescription)" } ?? "the video writer failed"
        }
        if let writeError {
            warn("video_write_error", "The video file may be incomplete: \(writeError)")
        }
        if let firstFrame, firstFrame > 30 {
            log.info("video: the call window appeared \(MicCapture.clock(firstFrame)) after recording started; the video is black until then")
        }
        let stats = Stats(
            file: url.lastPathComponent, width: Self.width, height: Self.height, framesPerSecond: Self.framesPerSecond,
            frames: frames, droppedFrames: dropped, durationSeconds: max(end, 0), firstFrameSeconds: firstFrame,
            windows: windows, writeError: writeError
        )
        log.info("video: \(frames) frames (\(dropped) dropped) from \(windows.isEmpty ? "no window" : windows.joined(separator: " → "))")
        return stats
    }

    // MARK: - Choosing the window

    private func rescan() {
        guard !stopped, !captureRevoked else { return }
        if case .selected(let filter, let label) = source {
            if stream == nil {
                if windows.isEmpty { windows.append(label) }
                startStream(filter)
            }
            return
        }
        SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: true) { [weak self] content, error in
            DispatchQueue.main.async {
                guard let self, !self.stopped else { return }
                if let error {
                    self.enumerationError = error.localizedDescription
                    self.problem = "Video sources couldn't be checked: \(error.localizedDescription)."
                    self.log.warn("video: couldn't list windows: \(error.localizedDescription)")
                    return
                }
                self.enumerationError = nil
                self.consider(content?.windows ?? [], displays: content?.displays ?? [])
            }
        }
    }

    private func consider(_ scWindows: [SCWindow], displays: [SCDisplay]) {
        let own = Bundle.main.bundleIdentifier
        let pairs: [(CallWindowCandidate, SCWindow)] = scWindows.compactMap { w in
            guard w.windowLayer == 0, let app = w.owningApplication, app.bundleIdentifier != own else { return nil }
            return (CallWindowCandidate(windowID: w.windowID, bundleID: app.bundleIdentifier, appName: app.applicationName,
                                        title: w.title ?? "", width: w.frame.width, height: w.frame.height), w)
        }
        let candidates = pairs.map(\.0)
        let fallback: Bool = if case .automaticWithScreenFallback = source { true } else { false }
        let target = AutomaticVideoSelection.choose(windows: candidates, current: current,
            displays: displays.map(\.displayID), mainDisplay: CGMainDisplayID(), allowScreenFallback: fallback)
        guard let target else {
            problem = "No meeting window detected; video isn't recording. Audio is recording. Stop and choose a window, app or screen."
            stream?.stopCapture { _ in }
            stream = nil
            current = nil
            return
        }
        if case .display = target {
            problem = "Recording your whole screen because no meeting window was detected."
        } else if captureError == nil {
            problem = nil
        }
        guard target != current || stream == nil else { return }
        let filter: SCContentFilter
        switch target {
        case .window(let chosen):
            guard let window = pairs.first(where: { $0.0.windowID == chosen.windowID })?.1 else { return }
            filter = SCContentFilter(desktopIndependentWindow: window)
        case .display(let id):
            guard let display = displays.first(where: { $0.displayID == id }) else { return }
            filter = SCContentFilter(display: display, excludingWindows: [])
            if !warnings.contains(where: { $0.code == "video_screen_fallback" }) {
                warn("video_screen_fallback", "The enabled screen fallback selected the whole screen while no meeting window was detected. See the video's source history for the sources captured.")
            }
        }
        current = target
        if windows.last != target.label { windows.append(target.label) }
        log.info("video: recording \(target.label)")
        if let stream {
            stream.updateContentFilter(filter) { [weak self] error in
                if let error {
                    DispatchQueue.main.async {
                        guard let self, !self.stopped, self.stream === stream, self.current == target else { return }
                        self.captureError = error.localizedDescription
                        self.captureRevoked = !VideoCaptureRecovery.shouldRetry(error)
                        self.problem = "Video couldn't switch sources: \(error.localizedDescription)."
                        self.stream?.stopCapture { _ in }
                        self.stream = nil  // retry the selected target on the next scan
                        self.log.warn("video: couldn't switch sources: \(error.localizedDescription)")
                    }
                }
            }
        } else {
            startStream(filter)
        }
    }

    private func startStream(_ filter: SCContentFilter) {
        let config = SCStreamConfiguration()
        config.width = Self.width
        config.height = Self.height
        config.scalesToFit = true
        config.preservesAspectRatio = true
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(Self.framesPerSecond))
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.showsCursor = false
        config.capturesAudio = false
        config.queueDepth = 6
        config.backgroundColor = .black
        let stream = SCStream(filter: filter, configuration: config, delegate: self)
        if case .selected = source {
            var pickerConfig = SCContentSharingPickerConfiguration()
            pickerConfig.allowsChangingSelectedContent = false
            SCContentSharingPicker.shared.setConfiguration(pickerConfig, for: stream)
        }
        do {
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        } catch {
            captureError = "\(error)"
            problem = "Video couldn't start: \(error.localizedDescription). Audio is recording."
            log.warn("video: couldn't add the stream output: \(error)")
            return
        }
        self.stream = stream
        stream.startCapture { [weak self] error in
            DispatchQueue.main.async {
                guard let self, !self.stopped, self.stream === stream else { return }
                guard let error else {
                    self.captureError = nil
                    if case .display = self.current {
                        self.problem = "Recording your whole screen because no meeting window was detected."
                    } else { self.problem = nil }
                    return
                }
                self.captureError = error.localizedDescription
                self.problem = "Video couldn't start: \(error.localizedDescription). Audio is recording."
                self.captureRevoked = !VideoCaptureRecovery.shouldRetry(error)
                self.log.warn("video: couldn't start capture: \(error.localizedDescription)")
                self.stream = nil  // the next rescan tries again
            }
        }
    }

    public func stream(_ stream: SCStream, didStopWithError error: Error) {
        DispatchQueue.main.async { [weak self] in
            guard let self, self.stream === stream, !self.stopped else { return }
            self.log.warn("video: capture stopped (\(error.localizedDescription)); looking for the call window again")
            self.captureError = error.localizedDescription
            self.captureRevoked = !VideoCaptureRecovery.shouldRetry(error)
            self.problem = self.captureRevoked
                ? "Video recording was stopped in macOS. Audio is still recording."
                : "Video capture stopped: \(error.localizedDescription). Trying to reconnect; audio is recording."
            self.stream = nil
        }
    }

    // MARK: - Writing (on `queue`)

    public func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, !queueStopped, sampleBuffer.isValid,
              let attachments = CMSampleBufferGetSampleAttachmentsArray(sampleBuffer, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let info = attachments.first,
              let rawStatus = info[.status] as? Int, SCFrameStatus(rawValue: rawStatus) == .complete,
              let pixels = sampleBuffer.imageBuffer
        else { return }
        let seconds: Double = if let host = info[.displayTime] as? UInt64 {
            AVAudioTime.seconds(forHostTime: host) - t0Seconds
        } else {
            sampleBuffer.presentationTimeStamp.seconds - t0Seconds
        }
        if writer == nil, writeError == nil {
            do { try makeWriter() } catch {
                writeError = "\(error)"
                log.error("video: couldn't create \(url.lastPathComponent): \(error)")
                DispatchQueue.main.async { [weak self] in
                    self?.problem = "The video file couldn't be written: \(error.localizedDescription). Audio is recording."
                }
                return
            }
        }
        guard let input, let adaptor, writer?.status == .writing else { return }
        guard let pts = clock.time(forSecondsSinceT0: seconds) else { return }
        guard input.isReadyForMoreMediaData else {
            dropped += 1
            return
        }
        if adaptor.append(pixels, withPresentationTime: pts) {
            frames += 1
            if firstFrame == nil { firstFrame = pts.seconds }
        } else {
            dropped += 1
        }
    }

    private func makeWriter() throws {
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        // Fragments every few seconds: a crash still leaves a playable file.
        writer.movieFragmentInterval = CMTime(seconds: 5, preferredTimescale: 600)
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: Self.width,
            AVVideoHeightKey: Self.height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: 900_000,
                AVVideoExpectedSourceFrameRateKey: Self.framesPerSecond,
                AVVideoMaxKeyFrameIntervalDurationKey: 2,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel,
                AVVideoAllowFrameReorderingKey: false,
            ],
        ])
        input.expectsMediaDataInRealTime = true
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: Self.width,
            kCVPixelBufferHeightKey as String: Self.height,
        ])
        guard writer.canAdd(input) else { throw CaptureError.message("the video writer won't take an H.264 input") }
        writer.add(input)
        guard writer.startWriting() else {
            throw CaptureError.message(writer.error.map { $0.localizedDescription } ?? "the video writer wouldn't start")
        }
        writer.startSession(atSourceTime: .zero)
        (self.writer, self.input, self.adaptor) = (writer, input, adaptor)
        // A black frame at 0, so the file starts at t0 like the audio does.
        if let pool = adaptor.pixelBufferPool {
            var black: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &black)
            if let black, clock.time(forSecondsSinceT0: 0) != nil {
                CVPixelBufferLockBaseAddress(black, [])
                memset(CVPixelBufferGetBaseAddress(black), 0, CVPixelBufferGetDataSize(black))
                CVPixelBufferUnlockBaseAddress(black, [])
                if adaptor.append(black, withPresentationTime: .zero) { frames += 1 }
            }
        }
    }

    private func warn(_ code: String, _ message: String) {
        log.warn(message)
        warnings.append(Issue(code: code, message: message))
    }
}
