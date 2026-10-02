import AVFoundation
import Foundation

/// Records the default microphone with AVAudioEngine (channel 0 only).
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
    private var engine = AVAudioEngine()
    private let writerLock = NSLock()
    private var writer: TrackWriter?
    private var t0Seconds = 0.0
    private var configObserver: NSObjectProtocol?
    private var running = false

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

    /// Starts the engine, then creates the track at the engine's actual input sample rate
    /// (voice processing can change it). Buffers that arrive before the writer exists are
    /// dropped; host-time alignment keeps the track correct regardless.
    public func start(t0HostTime: UInt64, aec: Bool, makeWriter: (Double) throws -> TrackWriter) throws {
        t0Seconds = AVAudioTime.seconds(forHostTime: t0HostTime)
        details["device"] = AVCaptureDevice.default(for: .audio)?.localizedName ?? "unknown"
        details["aec_requested"] = aec ? "yes" : "no"

        var format: AVAudioFormat
        do {
            format = try startEngine(aec: aec)
            details["aec_active"] = aec ? "yes" : "no"
        } catch where aec {
            let message = "echo cancellation (voice processing) failed to start: \(error); recording the raw mic instead"
            log.warn("mic: \(message)")
            warnings.append(message)
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            engine = AVAudioEngine()
            format = try startEngine(aec: false)
            details["aec_active"] = "no"
        }

        let writer = try makeWriter(format.sampleRate)
        writerLock.lock()
        self.writer = writer
        writerLock.unlock()
        running = true

        details["input_sample_rate"] = String(format.sampleRate)
        details["input_channels"] = String(format.channelCount)
        configObserver = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            self?.handleConfigurationChange()
        }
        log.info("mic: recording '\(details["device"] ?? "?")' at \(format.sampleRate) Hz, \(format.channelCount) ch, aec \(details["aec_active"] ?? "no")")
    }

    /// Stops the engine. Idempotent.
    public func stop() {
        guard running else { return }
        running = false
        if let observer = configObserver {
            NotificationCenter.default.removeObserver(observer)
            configObserver = nil
        }
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
    }

    private func startEngine(aec: Bool) throws -> AVAudioFormat {
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
        installTap(format: format)
        engine.prepare()
        try engine.start()
        return format
    }

    private func installTap(format: AVAudioFormat) {
        let t0 = t0Seconds
        engine.inputNode.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, when in
            guard let self, let channelData = buffer.floatChannelData else { return }
            self.writerLock.lock()
            let writer = self.writer
            self.writerLock.unlock()
            guard let writer else { return }
            let hostTime: UInt64? = when.isHostTimeValid ? when.hostTime : nil
            let start = hostTime.map { AVAudioTime.seconds(forHostTime: $0) - t0 }
            writer.append(UnsafeBufferPointer(start: channelData[0], count: Int(buffer.frameLength)), startSeconds: start, hostTime: hostTime)
        }
    }

    /// Device switches (e.g. plugging in a headset) stop the engine. Restart it when the sample
    /// rate is unchanged; gaps are filled with silence by host-time alignment.
    private func handleConfigurationChange() {
        guard running else { return }
        let format = engine.inputNode.outputFormat(forBus: 0)
        log.warn("mic: audio configuration changed (device switch?); input is now \(format.sampleRate) Hz, \(format.channelCount) ch")
        engine.inputNode.removeTap(onBus: 0)
        guard let writer, format.channelCount > 0, abs(format.sampleRate - writer.sampleRate) < 0.5 else {
            let message = "mic input changed to \(format.sampleRate) Hz mid-recording; mic capture stopped at that point"
            log.error("mic: \(message)")
            errors.append(message)
            return
        }
        installTap(format: format)
        do {
            engine.prepare()
            try engine.start()
            log.info("mic: restarted after configuration change")
        } catch {
            let message = "mic could not restart after a configuration change: \(error)"
            log.error("mic: \(message)")
            errors.append(message)
        }
    }
}
