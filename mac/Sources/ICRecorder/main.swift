import AppKit
import ICRecorderCore

let usage = """
usage: ICRecorder --session-dir <absolute dir> [--duration <seconds>] [--aec] [--video]

Records system audio (every app except this one) to <dir>/system.wav and the
microphone to <dir>/mic.wav until SIGINT/SIGTERM, a quit Apple event, or --duration.
With --video, also records the call's window (Zoom, Teams, Meet, …) to <dir>/video.mov;
that needs Screen Recording permission, and audio is recorded without it.
Writes <dir>/recorder.pid while running and <dir>/recorder.json when done.

Launch it as an app so macOS attributes the permissions to ICRecorder:
  open -n -a ICRecorder.app --args --session-dir /abs/path/session
"""

func die(_ message: String) -> Never {
    FileHandle.standardError.write("ICRecorder: \(message)\n\n\(usage)\n".data(using: .utf8)!)
    exit(2)
}

func parseOptions(_ arguments: [String]) -> RecorderOptions {
    var sessionDir: String?
    var duration: Double?
    var aec = false
    var video = false
    var index = 1
    func value(for flag: String) -> String {
        index += 1
        guard index < arguments.count else { die("missing value for \(flag)") }
        return arguments[index]
    }
    while index < arguments.count {
        let argument = arguments[index]
        switch argument {
        case "--session-dir":
            sessionDir = value(for: argument)
        case "--duration":
            guard let seconds = Double(value(for: argument)), seconds > 0 else { die("--duration must be a positive number of seconds") }
            duration = seconds
        case "--aec":
            aec = true
        case "--video":
            video = true
        case "-h", "--help":
            print(usage)
            exit(0)
        default:
            if argument.hasPrefix("-psn_") { break }  // process serial number some launch paths add
            die("unknown argument \(argument)")
        }
        index += 1
    }
    guard let sessionDir else { die("--session-dir is required") }
    guard sessionDir.hasPrefix("/") else { die("--session-dir must be absolute (apps launched with `open` start in /)") }
    return RecorderOptions(sessionDir: URL(fileURLWithPath: sessionDir, isDirectory: true), duration: duration, aec: aec, video: video)
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    let session: RecordingSession
    private var signalSources: [DispatchSourceSignal] = []

    init(session: RecordingSession) {
        self.session = session
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        for (number, name) in [(SIGINT, "SIGINT"), (SIGTERM, "SIGTERM")] {
            let source = DispatchSource.makeSignalSource(signal: number, queue: .main)
            source.setEventHandler { [weak self] in self?.session.stop(reason: name) }
            source.resume()
            signalSources.append(source)
        }
        session.start()
    }

    /// `osascript -e 'quit app "ICRecorder"'` lands here; finalize before exiting.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        session.stop(reason: "quit")
        return .terminateNow
    }
}

let options = parseOptions(CommandLine.arguments)
do {
    try FileManager.default.createDirectory(at: options.sessionDir, withIntermediateDirectories: true)
} catch {
    die("cannot create \(options.sessionDir.path): \(error)")
}
// Ignore default signal handling before anything starts; the dispatch sources handle them
// so both WAVs are always finalized.
signal(SIGINT, SIG_IGN)
signal(SIGTERM, SIG_IGN)

let log = Logger(fileURL: options.sessionDir.appendingPathComponent("recorder.log"))
let session = RecordingSession(options: options, log: log) { code in exit(code) }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = AppDelegate(session: session)
app.delegate = delegate
app.run()
