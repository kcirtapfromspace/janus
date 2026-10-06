import Foundation

/// The running app's release identity, including preview suffixes that the numeric
/// CFBundleShortVersionString cannot carry.
public struct AppVersion: Equatable, Sendable {
    public let release: String
    public let build: String

    public static var current: AppVersion { AppVersion(info: Bundle.main.infoDictionary ?? [:]) }

    public init(info: [String: Any]) {
        func value(_ key: String) -> String? {
            guard let text = info[key] as? String else { return nil }
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            return trimmed.isEmpty ? nil : trimmed
        }
        release = value("ICReleaseVersion") ?? value("CFBundleShortVersionString") ?? "Development"
        build = value("CFBundleVersion") ?? "unknown"
    }

    public var description: String { "Version \(release) (build \(build))" }
}
