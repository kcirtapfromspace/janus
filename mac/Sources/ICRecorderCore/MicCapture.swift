import AVFoundation
import Foundation

/// Records the default microphone with AVAudioEngine (channel 0 only), and keeps recording when
/// the input changes underneath it.
///
/// Switching to headphones, or a call app (Zoom, Meet, Teams) reconfiguring the microphone, posts
/// AVAudioEngineConfigurationChange, and Bluetooth headsets then move to a lower-rate call profile.
/// Restarting the same engine after that leaves it bound to the old device: "running" but
/// delivering nothing — which once cost a whole interview's worth of answers when someone put
/// their headphones on 25 s in. So every configuration change, and any 3 s stretch without audio
/// (the watchdog, which can't fail silently), builds a fresh engine on the current default input. Input at a different sample rate is converted to the track's rate. The writer's
/// host-time alignment fills any gap with silence, and every reconnect is logged and reported.
///
/// With `aec`, Apple's voice-processing I/O (echo cancellation) is enabled so interviewer audio
/// leaking from the speakers into the mic is suppressed. Voice processing ducks other apps' audio
/// by default; ducking is set to the minimum so the call stays audible. If voice processing fails
/// to start, capture falls back to the raw mic and records a warning.
public final class MicCapture {
    public private(set) var details: [String: String] = [:]
    public private(set) var warnings: [String] = []
    public private(set) var errors: [String] = []

    private let log: Logger
    private var engine: AVAudioEngine?
    private let writerLock = NSLock()
    private var writer: TrackWriter?
    private var t0Seconds = 0.0
    private var startUptime = 0.0
    private var configObserver: NSObjectProtocol?
    private var watchdog: Timer?
    private var running = false
    private var aec = false
    private var lastRebuildUptime = -Double.infinity
    /// Rebuilds since audio last arrived; after a few, retry less often so a dead mic doesn't spin.
    private var attemptsSinceAudio = 0
    private var reconnects = 0
    /// Times audio came back after a rebuild.
    private var restored = 0
    /// Every input used, in order, e.g. "MacBook Pro Microphone → AirPods Pro".
    private var devices: [String] = []
    /// Why the latest rebuild failed; reported only if the mic never came back.
    private var lastReconnectError: String?
    /// A fresh engine gets this long to start delivering before the watchdog judges it: Bluetooth
    /// mics take a moment, especially while switching to their call profile.
    private let rebuildGrace: TimeInterval = 5
    private var reconnectQueued = false

    public init(log: Logger) {
        self.log = log
    }

    /// Asks for microphone access if it hasn't been decided yet. Calls back on the main queue.
    public static func requestAccess(_ completion: @escaping (Bool) -> Void) {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized:
            completion(true)
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                DispatchQueue.main.async { completion(granted) }
            }
        default:
            completion(false)
        }
    }

    /// Starts capture, then creates the track at the input's actual sample rate (voice processing
    /// can change it). Buffers that arrive before the writer exists are dropped; host-time
    /// alignment keeps the track correct regardless. Call on the main thread.
    public func start(t0HostTime: UInt64, aec: Bool, makeWriter: (Double) throws -> TrackWriter) throws {
        t0Seconds = AVAudioTime.seconds(forHostTime: t0HostTime)
        startUptime = ProcessInfo.processInfo.systemUptime
        details["aec_requested"] = aec ? "yes" : "no"
        self.aec = aec
        let format = try buildEngineFallingBackFromAEC()

        let writer = try makeWriter(format.sampleRate)
        writerLock.lock()
        self.writer = writer
        writerLock.unlock()
        running = true

        details["input_sample_rate"] = String(format.sampleRate)
        details["input_channels"] = String(format.channelCount)
        let watchdog = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.checkFlow() }
        RunLoop.main.add(watchdog, forMode: .common)  // keeps ticking while a menu is open
        self.watchdog = watchdog
        log.info("mic: recording '\(details["device"] ?? "?")' at \(format.sampleRate) Hz, \(format.channelCount) ch, aec \(details["aec_active"] ?? "no")")
    }

    /// Stops capture. Idempotent.
    public func stop() {
        guard running else { return }
        running = false
        watchdog?.invalidate()
        watchdog = nil
        tearDownEngine()
        // Judge by whether audio was arriving at the end, not by the counters: a reconnect a
        // moment before stopping hasn't been counted as restored yet.
        writerLock.lock()
        let silentFor = writer?.secondsSinceLastAudio ?? .infinity
        writerLock.unlock()
        let deadAtEnd = attemptsSinceAudio > 0 && silentFor >= 3
        let recovered = restored + (attemptsSinceAudio > 0 && !deadAtEnd ? 1 : 0)
        if deadAtEnd {
            warnings.append("the microphone stopped delivering audio and didn't come back after \(attemptsSinceAudio) reconnect attempt(s)")
            if let lastReconnectError { errors.append(lastReconnectError) }
        } else if recovered > 0 {
            warnings.append("the microphone was reconnected \(recovered) time(s) during the recording; any gaps are filled with silence")
        }
    }

    // MARK: - Engine

    /// A fresh engine on the current input, with echo cancellation if requested and available.
    @discardableResult
    private func buildEngineFallingBackFromAEC() throws -> AVAudioFormat {
        do {
            let format = try buildEngine(aec: aec)
            details["aec_active"] = aec ? "yes" : "no"
            return format
        } catch where aec {
            let message = "echo cancellation (voice processing) failed to start: \(error); recording the raw mic instead"
            log.warn("mic: \(message)")
            warnings.append(message)
            aec = false
            details["aec_active"] = "no"
            return try buildEngine(aec: false)
        }
    }

    private func buildEngine(aec: Bool) throws -> AVAudioFormat {
        tearDownEngine()
        let engine = AVAudioEngine()
        let input = engine.inputNode
        if aec {
            try input.setVoiceProcessingEnabled(true)
            input.voiceProcessingOtherAudioDuckingConfiguration = .init(enableAdvancedDucking: false, duckingLevel: .min)
            _ = engine.mainMixerNode  // voice-processing I/O also needs the engine's output side configured
        }
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            throw CaptureError.message("no usable microphone input (format \(format))")
        }
        installTap(on: engine, format: format)
        engine.prepare()
        try engine.start()
        self.engine = engine
        let device = AVCaptureDevice.default(for: .audio)?.localizedName ?? "unknown"
        details["device"] = device
        if devices.last != device {
            devices.append(device)
            details["devices"] = devices.joined(separator: " → ")
        }
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            self?.reconnect(reason: "the audio input changed (headphones, or a call app taking over the mic)", queueIfTooSoon: true)
        }
        return format
    }

    private func tearDownEngine() {
        if let observer = configObserver {
            NotificationCenter.default.removeObserver(observer)
            configObserver = nil
        }
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
        engine = nil
    }

    private func installTap(on engine: AVAudioEngine, format: AVAudioFormat) {
        let t0 = t0Seconds
        var converter: AVAudioConverter?  // only used, and only touched on this tap's thread, if rates differ
        engine.inputNode.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, when in
            guard let self, let channelData = buffer.floatChannelData else { return }
            self.writerLock.lock()
            let writer = self.writer
            self.writerLock.unlock()
            guard let writer else { return }
            let hostTime: UInt64? = when.isHostTimeValid ? when.hostTime : nil
            let start = hostTime.map { AVAudioTime.seconds(forHostTime: $0) - t0 }
            if abs(buffer.format.sampleRate - writer.sampleRate) < 0.5 {
                writer.append(UnsafeBufferPointer(start: channelData[0], count: Int(buffer.frameLength)),
                              startSeconds: start, hostTime: hostTime)
            } else if let converted = Self.resample(buffer, to: writer.sampleRate, converter: &converter),
                      let samples = converted.floatChannelData {
                writer.append(UnsafeBufferPointer(start: samples[0], count: Int(converted.frameLength)),
                              startSeconds: start, hostTime: hostTime)
            }
        }
    }

    /// Converts a buffer's first channel to mono float32 at `rate` (e.g. a headset's 16 kHz call
    /// profile into a 48 kHz track). The converter keeps its state across buffers.
    static func resample(_ buffer: AVAudioPCMBuffer, to rate: Double, converter: inout AVAudioConverter?)
        -> AVAudioPCMBuffer? {
        guard let source = buffer.floatChannelData,
              let mono = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: buffer.format.sampleRate, channels: 1,
                                       interleaved: false),
              let target = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false),
              let input = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: buffer.frameLength),
              let inputSamples = input.floatChannelData
        else { return nil }
        input.frameLength = buffer.frameLength
        inputSamples[0].update(from: source[0], count: Int(buffer.frameLength))

        if converter == nil || converter?.inputFormat != mono || converter?.outputFormat != target {
            converter = AVAudioConverter(from: mono, to: target)
        }
        guard let converter else { return nil }
        let capacity = AVAudioFrameCount(Double(buffer.frameLength) * rate / buffer.format.sampleRate) + 64
        guard let output = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return nil }
        var supplied = false
        var error: NSError?
        converter.convert(to: output, error: &error) { _, status in
            if supplied {
                status.pointee = .noDataNow  // keep the stream open for the next buffer
                return nil
            }
            supplied = true
            status.pointee = .haveData
            return input
        }
        return error == nil ? output : nil
    }

    // MARK: - Recovery

    /// The configuration-change notification can fail to arrive or to help; this can't.
    /// No audio for 3 s → build a fresh engine. Runs every second on the main thread.
    private func checkFlow() {
        guard running else { return }
        writerLock.lock()
        let writer = self.writer
        writerLock.unlock()
        let now = ProcessInfo.processInfo.systemUptime
        let silentFor = writer?.secondsSinceLastAudio ?? (now - startUptime)
        if silentFor < 1 {
            if attemptsSinceAudio > 0 {
                restored += 1
                details["restored"] = String(restored)
                lastReconnectError = nil
                log.info("mic: audio is flowing again from '\(details["device"] ?? "?")'")
            }
            attemptsSinceAudio = 0
            return
        }
        if silentFor >= 3 && now - lastRebuildUptime >= rebuildGrace {
            reconnect(reason: "no microphone audio for \(Int(silentFor)) s")
        }
    }

    /// Rebuilds the engine on the current input. Rate-limited, and after a few attempts without
    /// any audio returning it backs off to every 30 s, so a dead mic doesn't spin.
    /// `queueIfTooSoon`: a configuration change right after a rebuild (a headset switching to its
    /// call profile) still needs its own rebuild, so it waits out the limit instead of being dropped.
    private func reconnect(reason: String, queueIfTooSoon: Bool = false) {
        guard running else { return }
        let now = ProcessInfo.processInfo.systemUptime
        let minimumInterval: Double = attemptsSinceAudio < 5 ? 2 : 30
        let wait = minimumInterval - (now - lastRebuildUptime)
        if wait > 0 {
            if queueIfTooSoon && !reconnectQueued {
                reconnectQueued = true
                DispatchQueue.main.asyncAfter(deadline: .now() + wait) { [weak self] in
                    self?.reconnectQueued = false
                    self?.reconnect(reason: reason)
                }
            }
            return
        }
        lastRebuildUptime = now
        attemptsSinceAudio += 1
        let at = Self.clock(now - startUptime)
        do {
            let format = try buildEngineFallingBackFromAEC()
            reconnects += 1
            details["reconnect_attempts"] = String(reconnects)
            details["last_reconnect"] = "\(at): \(reason)"
            log.warn("mic: \(reason) at \(at); reconnected to '\(details["device"] ?? "?")' at \(format.sampleRate) Hz")
        } catch {
            // Common mid-switch (the new device isn't ready yet); the watchdog retries.
            let message = "\(reason) at \(at); could not reconnect the microphone: \(error)"
            log.error("mic: \(message)")
            lastReconnectError = message
        }
    }

    static func clock(_ seconds: Double) -> String {
        let s = max(0, Int(seconds))
        return s >= 3600 ? String(format: "%d:%02d:%02d", s / 3600, s % 3600 / 60, s % 60) : String(format: "%d:%02d", s / 60, s % 60)
    }
}
