import CoreAudio
import Foundation

public enum CaptureError: Error, CustomStringConvertible {
    case coreAudio(String, OSStatus)
    case message(String)

    public var description: String {
        switch self {
        case let .coreAudio(what, status):
            return "\(what) failed (OSStatus \(fourCC(UInt32(bitPattern: status))))"
        case let .message(text):
            return text
        }
    }
}

func check(_ status: OSStatus, _ what: @autoclosure () -> String) throws {
    guard status == noErr else { throw CaptureError.coreAudio(what(), status) }
}

/// Renders Core Audio codes as 'abcd' when printable, otherwise as a number.
func fourCC(_ value: UInt32) -> String {
    let bytes = [24, 16, 8, 0].map { UInt8((value >> UInt32($0)) & 0xFF) }
    if bytes.allSatisfy({ $0 >= 32 && $0 < 127 }), let text = String(bytes: bytes, encoding: .ascii) {
        return "'\(text)'"
    }
    return "\(Int32(bitPattern: value))"
}

/// Appends timestamped lines to recorder.log. stdout is invisible when the app is launched
/// with `open -a`, so the log file is the only place to see what happened; lines are also
/// mirrored to stderr for runs from a terminal.
public final class Logger {
    private let lock = NSLock()
    private var handle: FileHandle?
    private let formatter: ISO8601DateFormatter = {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }()

    public init(fileURL: URL?) {
        guard let url = fileURL else { return }
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        handle = try? FileHandle(forWritingTo: url)
        _ = try? handle?.seekToEnd()
    }

    public func info(_ message: String) { write("INFO", message) }
    public func warn(_ message: String) { write("WARN", message) }
    public func error(_ message: String) { write("ERROR", message) }

    private func write(_ level: String, _ message: String) {
        lock.lock()
        defer { lock.unlock() }
        let line = "\(formatter.string(from: Date())) [\(level)] \(message)\n"
        guard let data = line.data(using: .utf8) else { return }
        try? handle?.write(contentsOf: data)
        FileHandle.standardError.write(data)
    }
}

/// The running app's name ("ICRecorder" or "Interview Coach"), for permission messages.
public let appName: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleName") as? String ?? "ICRecorder"
