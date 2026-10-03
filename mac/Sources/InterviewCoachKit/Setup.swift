import Foundation

/// `ic setup status --json`: what this Mac still needs, as rows for the Setup window.
public struct SetupStatus: Decodable, Equatable {
    public let ready: Bool
    /// Required checks still to do.
    public let remaining: Int
    public let model: String
    public let checks: [SetupCheck]

    public func check(_ id: String) -> SetupCheck? { checks.first { $0.id == id } }
}

public struct SetupCheck: Decodable, Equatable, Identifiable {
    public enum Status: String, Decodable {
        case ok, action, blocked, optional
    }

    public let id: String
    public let status: Status
    public let required: Bool
    public let title: String
    public let detail: String
    public let action: SetupAction?

    public var isDone: Bool { status == .ok }
}

/// What a check's button does.
public struct SetupAction: Decodable, Equatable {
    public enum Kind: Equatable {
        /// `ic setup run <step> --events`
        case run(step: String)
        case openURL(URL)
        /// `ic login --events`
        case signIn
        /// `ic login --switch --events`: sign out, then sign in with another account
        case switchAccount
        /// `ic proxy key <target> --stdin`
        case key(target: String)
    }

    public let label: String
    public let kind: Kind

    private enum CodingKeys: String, CodingKey {
        case label, kind, step, url, target
    }

    public init(label: String, kind: Kind) {
        self.label = label
        self.kind = kind
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        label = try c.decode(String.self, forKey: .label)
        switch try c.decode(String.self, forKey: .kind) {
        case "run": kind = .run(step: try c.decode(String.self, forKey: .step))
        case "open_url":
            let text = try c.decode(String.self, forKey: .url)
            guard let url = URL(string: text) else {
                throw DecodingError.dataCorruptedError(forKey: .url, in: c, debugDescription: "bad URL \(text)")
            }
            kind = .openURL(url)
        case "sign_in": kind = .signIn
        case "switch_account": kind = .switchAccount
        case "key": kind = .key(target: try c.decode(String.self, forKey: .target))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: c, debugDescription: "unknown action \(other)")
        }
    }
}

/// One line of `--events` output from `ic setup run` or `ic login`.
public enum SetupEvent: Equatable {
    case stage(String)
    case progress(done: Int64, total: Int64)
    case openURL(URL)
    case needCode
    case log(String)
    case done
    case error(String)

    public static func parse(_ line: String) -> SetupEvent? {
        guard let data = line.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let event = object["event"] as? String
        else { return nil }
        let message = object["message"] as? String ?? ""
        switch event {
        case "stage": return .stage(message)
        case "progress":
            let number = { (key: String) in (object[key] as? NSNumber)?.int64Value ?? 0 }
            return .progress(done: number("done"), total: number("total"))
        case "open_url": return (object["url"] as? String).flatMap(URL.init(string:)).map(SetupEvent.openURL)
        case "need_code": return .needCode
        case "log": return .log(message)
        case "done": return .done
        case "error": return .error(message)
        default: return nil
        }
    }
}

/// Splits a byte stream into lines as it arrives (a line can span several reads).
public struct LineSplitter {
    private var pending = Data()

    public init() {}

    public mutating func feed(_ data: Data) -> [String] {
        pending.append(data)
        var lines: [String] = []
        while let newline = pending.firstIndex(of: UInt8(ascii: "\n")) {
            lines.append(String(decoding: pending[pending.startIndex..<newline], as: UTF8.self))
            pending.removeSubrange(pending.startIndex...newline)
        }
        return lines
    }

    /// Whatever is left after the stream ends (a last line without a newline).
    public mutating func finish() -> String? {
        defer { pending.removeAll() }
        return pending.isEmpty ? nil : String(decoding: pending, as: UTF8.self)
    }
}
