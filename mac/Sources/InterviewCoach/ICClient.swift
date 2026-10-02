import Foundation

/// Runs the `ic` command-line tool, which owns all session data, transcription, and analysis.
/// The app only records audio and shows results.
struct ICClient: Sendable {
    let executable: URL

    /// The copy bundled inside the app first, then a `cargo install`ed one.
    static func locate() -> ICClient? {
        let candidates = [
            Bundle.main.url(forAuxiliaryExecutable: "ic"),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".cargo/bin/ic"),
        ]
        return candidates.compactMap { $0 }
            .first { FileManager.default.isExecutableFile(atPath: $0.path) }
            .map(ICClient.init(executable:))
    }

    struct Failure: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    /// GUI apps don't inherit the shell's PATH, and ic shells out to ffmpeg, docker, ant, and tar.
    private static let environment: [String: String] = {
        var env = ProcessInfo.processInfo.environment
        let home = NSHomeDirectory()
        env["PATH"] = ["/opt/homebrew/bin", "/usr/local/bin", "\(home)/.cargo/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"]
            .joined(separator: ":")
        return env
    }()

    /// Run `ic <arguments>` off the main thread; returns stdout, or throws with ic's error message.
    func run(_ arguments: [String]) async throws -> String {
        let executable = self.executable
        return try await Task.detached {
            let process = Process()
            process.executableURL = executable
            process.arguments = arguments
            process.environment = Self.environment
            process.standardInput = FileHandle.nullDevice
            let out = Pipe(), err = Pipe()
            process.standardOutput = out
            process.standardError = err
            try process.run()
            // Drain both pipes concurrently so a large output can't fill a pipe and stall ic.
            async let stdout = Task.detached { out.fileHandleForReading.readDataToEndOfFile() }.value
            async let stderr = Task.detached { err.fileHandleForReading.readDataToEndOfFile() }.value
            let (outData, errData) = await (stdout, stderr)
            process.waitUntilExit()
            guard process.terminationStatus == 0 else {
                throw Failure(message: Self.errorMessage(String(decoding: errData, as: UTF8.self)))
            }
            return String(decoding: outData, as: UTF8.self)
        }.value
    }

    func decode<T: Decodable>(_ type: T.Type, _ arguments: [String]) async throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: Data(try await run(arguments).utf8))
    }

    /// ic prints "Error: <message>" (possibly multi-line) as its last stderr output.
    private static func errorMessage(_ stderr: String) -> String {
        if let range = stderr.range(of: "Error: ", options: .backwards) {
            return String(stderr[range.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        let trimmed = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "ic failed without an error message" : trimmed
    }
}

// MARK: - JSON shapes (see `ic list --json`, `ic doctor --json`, `ic recording begin`)

struct SessionSummary: Decodable, Identifiable, Hashable {
    let id: Int
    let createdAt: String
    let title: String
    let company: String?
    let stage: String?
    let status: String
    let mode: String
    let durationS: Double?
    let dir: String
    let verdict: String?
    let verdictLabel: String?
    let outcome: String?
    let outcomeLabel: String?
    let reportPath: String?
    let transcriptPath: String?
    let error: String?

    var date: String { String(createdAt.prefix(10)) }
    var isAnalyzed: Bool { reportPath != nil }
}

struct Health: Decodable {
    let model: String
    let signedIn: Bool
    let dockerRunning: Bool
    let proxyReady: Bool
    /// Signing in (which also starts the LLM proxy) would fix the current problem.
    let needsLogin: Bool
    let problems: [String]
}

struct NewRecording: Decodable {
    let id: Int
    let dir: String
}

/// Values `ic outcome` accepts, with their labels.
let outcomeChoices: [(value: String, label: String)] = [
    ("pending", "Waiting to hear"), ("advanced", "Advanced"), ("offer", "Offer"),
    ("rejected", "Rejected"), ("withdrew", "Withdrew"), ("no_response", "No response"),
]
