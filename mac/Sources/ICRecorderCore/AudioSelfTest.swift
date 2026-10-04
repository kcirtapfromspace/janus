import Foundation

/// A 5-second test of both tracks, run when you click "Test recording" in Setup. It catches a
/// missing permission before a real interview instead of after it.
///
/// macOS has no way to ask whether System Audio Recording is allowed: a denied tap just delivers
/// silence. So the test plays a short system sound and checks that the call-audio track heard it.
/// The sound is played by `afplay`, a separate process, because the tap leaves out the app's own
/// audio. The mic track only has to deliver real (non-zero) samples; room noise is enough.
public final class AudioSelfTest {
    public struct Result: Equatable {
        public var micPeak: Double
        public var systemPeak: Double
        public var micDelivered: Bool
        public var issues: [String]

        public var micOK: Bool { micDelivered && micPeak > 0 }
        public var systemOK: Bool { systemPeak > 0 }

        public init(micPeak: Double, systemPeak: Double, micDelivered: Bool, issues: [String]) {
            self.micPeak = micPeak
            self.systemPeak = systemPeak
            self.micDelivered = micDelivered
            self.issues = issues
        }
    }

    private var session: RecordingSession?
    private var sound: Process?
    private let directory: URL

    public init() {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("interview-coach-self-test-\(UUID().uuidString)", isDirectory: true)
    }

    /// Records for `seconds`, then calls back on the main queue. Asks for microphone access first if
    /// it hasn't been decided. Call on the main thread.
    public func run(seconds: Double = 5, completion: @escaping (Result) -> Void) {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        } catch {
            completion(Result(micPeak: 0, systemPeak: 0, micDelivered: false, issues: ["Couldn't create a test folder: \(error)"]))
            return
        }
        let session = RecordingSession(
            options: RecorderOptions(sessionDir: directory, duration: seconds, aec: false),
            log: Logger(fileURL: directory.appendingPathComponent("recorder.log")),
            onExit: { [weak self] _ in
                DispatchQueue.main.async {
                    guard let self else { return }
                    completion(self.readResult())
                    self.cleanUp()
                }
            }
        )
        self.session = session
        session.start()
        // Give both tracks a second to start, then make a sound only the call-audio track can hear.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self, self.session != nil else { return }
            let sound = Process()
            sound.executableURL = URL(fileURLWithPath: "/usr/bin/afplay")
            sound.arguments = ["/System/Library/Sounds/Glass.aiff"]
            try? sound.run()
            self.sound = sound
        }
    }

    public func cancel() {
        session?.stop(reason: "cancelled")
    }

    private func readResult() -> Result {
        Self.result(fromReport: (try? Data(contentsOf: directory.appendingPathComponent("recorder.json"))) ?? Data())
    }

    /// The verdict from a recorder.json (a pure function, so it's unit-tested).
    public static func result(fromReport data: Data) -> Result {
        guard let report = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return Result(micPeak: 0, systemPeak: 0, micDelivered: false, issues: ["The test didn't produce a result."])
        }
        let tracks = report["tracks"] as? [String: [String: Any]] ?? [:]
        let peak = { (name: String) in (tracks[name]?["peak"] as? NSNumber)?.doubleValue ?? 0 }
        let captured = { (name: String) in (tracks[name]?["captured_frames"] as? NSNumber)?.int64Value ?? 0 }
        let messages = ((report["errors"] as? [[String: Any]] ?? []) + (report["warnings"] as? [[String: Any]] ?? []))
            .compactMap { $0["message"] as? String }
        return Result(micPeak: peak("mic"), systemPeak: peak("system"), micDelivered: captured("mic") > 0, issues: messages)
    }

    private func cleanUp() {
        if sound?.isRunning == true { sound?.terminate() }
        sound = nil
        session = nil
        try? FileManager.default.removeItem(at: directory)
    }
}
