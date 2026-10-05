import AVFoundation
import CoreAudio
import Foundation

/// Records a mock interview the way RecordingSession records a real one, as two tracks from t0:
/// your mic to mic.wav, and the interviewer's synthesized voice to system.wav at the moment it
/// plays. `ic recording finish` then reviews it like any interview.
///
/// The voice plays through the same audio engine as the mic, with voice processing (echo
/// cancellation) on, so it's taken out of your track; headphones are still clearer. Each answer
/// is also kept on its own (answer-N.wav), so the interviewer can hear it straight away.
/// All methods run on the main queue.
public final class MockRecorder {
    public let sessionDir: URL
    private let log: Logger
    private let engine = AVAudioEngine()
    private let player = AVAudioPlayerNode()
    private let synthesizer = AVSpeechSynthesizer()
    /// The voice track's format: what every synthesized buffer is converted to.
    private let voiceFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 24000, channels: 1, interleaved: false)!
    private var micWriter: TrackWriter?
    private var voiceWriter: TrackWriter?
    private var t0HostTime: UInt64 = 0
    private var t0Seconds = 0.0
    private var startedAt: Date?
    private var echoCancellation = false
    private var finished = false
    private var answers = 0
    private var micRate = 48000.0
    // Shared with the audio thread.
    private let lock = NSLock()
    private var answer: [Float]?
    private var peak: Float = 0

    private var pidURL: URL { sessionDir.appendingPathComponent("recorder.pid") }

    public init(sessionDir: URL, log: Logger) {
        self.sessionDir = sessionDir
        self.log = log
    }

    /// The clearest installed English voice: premium or enhanced if you've downloaded one
    /// (System Settings › Accessibility › Spoken Content), never a novelty voice.
    public static var voice: AVSpeechSynthesisVoice? {
        let english = AVSpeechSynthesisVoice.speechVoices().filter {
            $0.language.hasPrefix("en") && !$0.voiceTraits.contains(.isNoveltyVoice)
        }
        let rank = { (v: AVSpeechSynthesisVoice) in (v.quality.rawValue, v.language == "en-US" ? 1 : 0, v.name == "Samantha" ? 1 : 0) }
        return english.max { rank($0) < rank($1) } ?? AVSpeechSynthesisVoice(language: "en-US")
    }

    /// The mic's loudness since the last call (0…1), for a level meter.
    public func takeLevel() -> Float {
        lock.lock()
        defer { lock.unlock() }
        let level = peak
        peak = 0
        return level
    }

    public func start() throws {
        let fm = FileManager.default
        for name in ["mic.wav", "system.wav"] where fm.fileExists(atPath: sessionDir.appendingPathComponent(name).path) {
            throw CaptureError.message("\(name) already exists; refusing to overwrite an earlier recording")
        }
        try "\(getpid())\n".write(to: pidURL, atomically: true, encoding: .utf8)
        let input = engine.inputNode
        do {
            try input.setVoiceProcessingEnabled(true)
            input.voiceProcessingOtherAudioDuckingConfiguration = .init(enableAdvancedDucking: false, duckingLevel: .min)
            echoCancellation = true
        } catch {
            log.warn("mock: echo cancellation unavailable (\(error)); headphones keep the interviewer out of your track")
        }
        engine.attach(player)
        engine.connect(player, to: engine.mainMixerNode, format: voiceFormat)
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            try? fm.removeItem(at: pidURL)
            throw CaptureError.message("no usable microphone input (format \(format))")
        }
        micRate = format.sampleRate
        micWriter = try TrackWriter(url: sessionDir.appendingPathComponent("mic.wav"), sampleRate: micRate)
        voiceWriter = try TrackWriter(url: sessionDir.appendingPathComponent("system.wav"), sampleRate: voiceFormat.sampleRate)
        t0HostTime = AudioGetCurrentHostTime()
        t0Seconds = AVAudioTime.seconds(forHostTime: t0HostTime)
        startedAt = Date()
        let t0 = t0Seconds
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, when in
            guard let self, let channels = buffer.floatChannelData else { return }
            let samples = UnsafeBufferPointer(start: channels[0], count: Int(buffer.frameLength))
            let hostTime: UInt64? = when.isHostTimeValid ? when.hostTime : nil
            self.micWriter?.append(samples, startSeconds: hostTime.map { AVAudioTime.seconds(forHostTime: $0) - t0 }, hostTime: hostTime)
            let loudest = samples.reduce(Float(0)) { max($0, abs($1)) }
            self.lock.lock()
            self.answer?.append(contentsOf: samples)
            self.peak = max(self.peak, loudest)
            self.lock.unlock()
        }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            try? fm.removeItem(at: pidURL)
            throw error
        }
        player.play()
        log.info("mock: recording started (echo cancellation \(echoCancellation ? "on" : "off"), voice \(Self.voice?.name ?? "default"))")
    }

    /// Say `text` in the interviewer's voice, recording it on the voice track as it plays. `done`
    /// runs on the main queue once it has been heard.
    public func speak(_ text: String, done: @escaping () -> Void) {
        let utterance = AVSpeechUtterance(string: text)
        utterance.voice = Self.voice
        var pieces: [AVAudioPCMBuffer] = []
        synthesizer.write(utterance) { [weak self] buffer in
            guard let pcm = buffer as? AVAudioPCMBuffer else { return }
            if pcm.frameLength > 0 {
                pieces.append(pcm)
                return
            }
            let all = pieces  // the last, empty buffer: the utterance is complete
            DispatchQueue.main.async { self?.play(all, done: done) }
        }
    }

    private func play(_ pieces: [AVAudioPCMBuffer], done: @escaping () -> Void) {
        guard !finished, let speech = convert(pieces), speech.frameLength > 0, let channel = speech.floatChannelData else {
            done()
            return
        }
        // Written where it will be heard: now, plus the output's latency.
        let at = AVAudioTime.seconds(forHostTime: AudioGetCurrentHostTime()) - t0Seconds + engine.outputNode.presentationLatency
        voiceWriter?.append(UnsafeBufferPointer(start: channel[0], count: Int(speech.frameLength)), startSeconds: at)
        player.scheduleBuffer(speech, completionCallbackType: .dataPlayedBack) { _ in
            DispatchQueue.main.async { done() }
        }
        if !player.isPlaying { player.play() }
    }

    private func convert(_ pieces: [AVAudioPCMBuffer]) -> AVAudioPCMBuffer? {
        Self.convert(pieces, to: voiceFormat, log: log)
    }

    /// The synthesizer's buffers, joined and converted to the voice track's format.
    static func convert(_ pieces: [AVAudioPCMBuffer], to voiceFormat: AVAudioFormat, log: Logger) -> AVAudioPCMBuffer? {
        guard let first = pieces.first, let converter = AVAudioConverter(from: first.format, to: voiceFormat) else { return nil }
        let frames = pieces.reduce(0) { $0 + Double($1.frameLength) }
        let capacity = AVAudioFrameCount(frames * voiceFormat.sampleRate / first.format.sampleRate) + 4096
        guard let out = AVAudioPCMBuffer(pcmFormat: voiceFormat, frameCapacity: capacity) else { return nil }
        var next = 0
        var error: NSError?
        converter.convert(to: out, error: &error) { _, status in
            guard next < pieces.count else {
                status.pointee = .endOfStream
                return nil
            }
            status.pointee = .haveData
            next += 1
            return pieces[next - 1]
        }
        if let error { log.warn("mock: couldn't convert the interviewer's voice: \(error)") }
        return out
    }

    /// Start keeping your answer apart from the rest of the mic track.
    public func beginAnswer() {
        lock.lock()
        answer = []
        lock.unlock()
    }

    /// Your answer since `beginAnswer`, written to answer-N.wav for the interviewer to hear.
    public func endAnswer() throws -> URL {
        lock.lock()
        let samples = answer ?? []
        answer = nil
        lock.unlock()
        answers += 1
        let url = sessionDir.appendingPathComponent("answer-\(answers).wav")
        let writer = try TrackWriter(url: url, sampleRate: micRate)
        samples.withUnsafeBufferPointer { writer.append($0, startSeconds: nil) }
        writer.finish()
        return url
    }

    /// Stop and write recorder.json, as RecordingSession does. Returns false if it couldn't be written.
    @discardableResult
    public func stop(reason: String) -> Bool {
        guard !finished else { return true }
        finished = true
        synthesizer.stopSpeaking(at: .immediate)
        player.stop()
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        let end = AVAudioTime.seconds(forHostTime: AudioGetCurrentHostTime()) - t0Seconds
        // The voice track runs to the end too: silence after the interviewer's last words.
        let zero: [Float] = [0]
        zero.withUnsafeBufferPointer { voiceWriter?.append($0, startSeconds: end) }
        var tracks: [String: TrackWriter.Stats] = [:]
        if var mic = micWriter?.finish() {
            mic.details = ["device": "default input", "echo_cancellation": echoCancellation ? "yes" : "no"]
            tracks["mic"] = mic
        }
        if var voice = voiceWriter?.finish() {
            // Silence between the interviewer's turns is the conversation, not a dropout.
            voice.gapFillFrames = 0
            voice.gapFillEvents = 0
            voice.details = ["source": "mock interviewer (speech synthesis)", "voice": Self.voice?.name ?? "default"]
            tracks["system"] = voice
        }
        let stamp = ISO8601DateFormatter()
        let report = RecorderReport(
            sessionDir: sessionDir.path,
            startedAt: startedAt.map { stamp.string(from: $0) },
            stoppedAt: stamp.string(from: Date()),
            stopReason: reason,
            requestedDurationSeconds: nil,
            t0HostTime: t0HostTime,
            tracks: tracks,
            warnings: [],
            errors: []
        )
        var written = true
        do {
            let encoder = JSONEncoder()
            encoder.keyEncodingStrategy = .convertToSnakeCase
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(report).write(to: sessionDir.appendingPathComponent("recorder.json"), options: .atomic)
        } catch {
            log.error("mock: could not write recorder.json: \(error)")
            written = false
        }
        try? FileManager.default.removeItem(at: pidURL)
        log.info("mock: stopped (\(reason)), \(answers) answers")
        return written
    }
}
