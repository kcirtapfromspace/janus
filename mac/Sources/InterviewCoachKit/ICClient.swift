import Foundation

/// Runs the `ic` command-line tool, which owns all session data, transcription, and analysis.
/// The app only records audio and shows results.
public struct ICClient: Sendable {
    public let executable: URL

    public init(executable: URL) {
        self.executable = executable
    }

    /// The copy bundled inside the app first, then a `cargo install`ed one.
    public static func locate() -> ICClient? {
        let candidates = [
            Bundle.main.url(forAuxiliaryExecutable: "ic"),
            URL(fileURLWithPath: NSHomeDirectory()).appendingPathComponent(".cargo/bin/ic"),
        ]
        return candidates.compactMap { $0 }
            .first { FileManager.default.isExecutableFile(atPath: $0.path) }
            .map(ICClient.init(executable:))
    }

    public struct Failure: LocalizedError {
        public let message: String
        public var errorDescription: String? { message }
    }

    /// ic finds its own tools (the bundled ffmpeg and ant, Docker wherever it's installed), so the
    /// app passes only the system PATH; nothing depends on Homebrew.
    static let environment: [String: String] = {
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
        return env
    }()

    /// Run `ic <arguments>` off the main thread; returns stdout, or throws with ic's error message.
    /// `stdin` is written to ic's standard input (e.g. an API key, so it never appears in arguments).
    public func run(_ arguments: [String], stdin: String? = nil) async throws -> String {
        let executable = self.executable
        return try await Task.detached {
            let process = Process()
            process.executableURL = executable
            process.arguments = arguments
            process.environment = Self.environment
            let input = Pipe()
            process.standardInput = stdin == nil ? FileHandle.nullDevice : input
            let out = Pipe(), err = Pipe()
            process.standardOutput = out
            process.standardError = err
            try process.run()
            if let stdin {
                try? input.fileHandleForWriting.write(contentsOf: Data((stdin + "\n").utf8))
                try? input.fileHandleForWriting.close()
            }
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

    public func decode<T: Decodable>(_ type: T.Type, _ arguments: [String]) async throws -> T {
        try Self.decode(T.self, from: Data(try await run(arguments).utf8))
    }

    /// ic's JSON uses snake_case keys.
    public static func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(T.self, from: data)
    }

    /// Start `ic <arguments>` and deliver its `--events` lines as they arrive, on the main actor.
    /// Its stdin stays open so a line can be sent to it (a sign-in code) until it ends or is cancelled.
    public func stream(_ arguments: [String], onEvent: @escaping @MainActor (SetupEvent) -> Void) throws -> ICStream {
        try ICStream(executable: executable, arguments: arguments, onEvent: onEvent)
    }

    /// ic prints "Error: <message>" (possibly multi-line) as its last stderr output.
    static func errorMessage(_ stderr: String) -> String {
        if let range = stderr.range(of: "Error: ", options: .backwards) {
            return String(stderr[range.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
        }
        let trimmed = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "ic failed without an error message" : trimmed
    }
}

// MARK: - JSON shapes (see `ic list --json`, `ic recording begin`; setup is in Setup.swift)

public struct SessionSummary: Decodable, Identifiable, Hashable {
    public let id: Int
    public let createdAt: String
    public let title: String
    public let company: String?
    public let stage: String?
    public let status: String
    public let mode: String
    public let durationS: Double?
    public let dir: String
    public let verdict: String?
    public let verdictLabel: String?
    public let outcome: String?
    public let outcomeLabel: String?
    public let reportPath: String?
    public let transcriptPath: String?
    public let error: String?

    public var date: String { String(createdAt.prefix(10)) }
    public var isAnalyzed: Bool { reportPath != nil }
}

public struct NewRecording: Decodable {
    public let id: Int
    public let dir: String
}

/// Values `ic outcome` accepts, with their labels.
public let outcomeChoices: [(value: String, label: String)] = [
    ("pending", "Waiting to hear"), ("advanced", "Advanced"), ("offer", "Offer"),
    ("rejected", "Rejected"), ("withdrew", "Withdrew"), ("no_response", "No response"),
]


/// A running `ic … --events` command.
public final class ICStream: @unchecked Sendable {
    private let process = Process()
    private let input = Pipe()
    private let lock = NSLock()
    private var splitter = LineSplitter()
    private var stderr = Data()
    private var lastError: String?
    private var finished: CheckedContinuation<Void, Error>?
    private var exitStatus: Int32?

    init(executable: URL, arguments: [String], onEvent: @escaping @MainActor (SetupEvent) -> Void) throws {
        process.executableURL = executable
        process.arguments = arguments
        process.environment = ICClient.environment
        process.standardInput = input
        let out = Pipe(), err = Pipe()
        process.standardOutput = out
        process.standardError = err
        out.fileHandleForReading.readabilityHandler = { [weak self] handle in
            guard let self else { return }
            let data = handle.availableData
            self.lock.lock()
            let lines = data.isEmpty ? [self.splitter.finish()].compactMap { $0 } : self.splitter.feed(data)
            self.lock.unlock()
            if data.isEmpty { handle.readabilityHandler = nil }
            for event in lines.compactMap(SetupEvent.parse) {
                if case .error(let message) = event { self.withLock { self.lastError = message } }
                Task { @MainActor in onEvent(event) }
            }
        }
        err.fileHandleForReading.readabilityHandler = { [weak self] handle in
            let data = handle.availableData
            if data.isEmpty { handle.readabilityHandler = nil }
            self?.withLock { self?.stderr.append(data) }
        }
        process.terminationHandler = { [weak self] process in
            guard let self else { return }
            let continuation = self.withLock { () -> CheckedContinuation<Void, Error>? in
                self.exitStatus = process.terminationStatus
                defer { self.finished = nil }
                return self.finished
            }
            continuation.map(self.resume)
        }
        try process.run()
    }

    private func withLock<T>(_ body: () -> T) -> T {
        lock.lock()
        defer { lock.unlock() }
        return body()
    }

    private func resume(_ continuation: CheckedContinuation<Void, Error>) {
        let (status, message) = withLock { (exitStatus ?? 1, lastError ?? ICClient.errorMessage(String(decoding: stderr, as: UTF8.self))) }
        if status == 0 { continuation.resume() } else { continuation.resume(throwing: ICClient.Failure(message: message)) }
    }

    /// Send one line to ic's stdin (e.g. the code a sign-in page shows).
    public func send(_ line: String) {
        try? input.fileHandleForWriting.write(contentsOf: Data((line + "\n").utf8))
    }

    public func cancel() {
        if process.isRunning { process.terminate() }
    }

    /// Wait for ic to finish; throws with its error message if it failed or was cancelled.
    public func wait() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            let alreadyDone = withLock { () -> Bool in
                if exitStatus != nil { return true }
                finished = continuation
                return false
            }
            if alreadyDone { resume(continuation) }
        }
    }
}
