import ScreenCaptureKit
import Foundation

/// A picked filter is authorized by the macOS picker and bypasses meeting-title detection.
public enum VideoSource {
    case automatic
    case automaticWithScreenFallback
    case selected(filter: SCContentFilter, label: String)
}

enum AutomaticVideoTarget: Equatable {
    case window(CallWindowCandidate)
    case display(UInt32)

    var label: String {
        switch self {
        case .window(let window): return window.label
        case .display(let id): return "Whole screen (display \(id))"
        }
    }
}

/// Keep the chosen window when its tab title changes. Expand to a display only with explicit opt-in.
enum AutomaticVideoSelection {
    static func choose(windows: [CallWindowCandidate], current: AutomaticVideoTarget?,
                       displays: [UInt32], mainDisplay: UInt32, allowScreenFallback: Bool) -> AutomaticVideoTarget? {
        let best = CallWindows.choose(windows)
        if case .window(let window) = current,
           CallWindows.canKeep(window, available: windows) {
            if CallWindows.shouldSwitch(from: window, to: best, available: windows), let best {
                return .window(best)
            }
            return current
        }
        if let best { return .window(best) }
        guard allowScreenFallback else { return nil }
        if case .display(let id) = current, displays.contains(id) { return current }
        guard let id = displays.contains(mainDisplay) ? mainDisplay : displays.sorted().first else { return nil }
        return .display(id)
    }
}

enum VideoCaptureRecovery {
    static func shouldRetry(_ error: Error) -> Bool {
        let nsError = error as NSError
        guard nsError.domain == SCStreamErrorDomain else { return true }
        return nsError.code != SCStreamError.Code.userStopped.rawValue
            && nsError.code != SCStreamError.Code.userDeclined.rawValue
    }
}
