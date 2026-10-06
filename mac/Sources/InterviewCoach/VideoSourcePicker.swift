import AppKit
import ScreenCaptureKit

/// Selection only: presenting this picker never starts recording.
@MainActor
final class VideoSourcePicker: NSObject, SCContentSharingPickerObserver {
    var onSelection: ((SCContentFilter, String) -> Void)?
    var onCancel: (() -> Void)?
    var onError: ((String) -> Void)?
    private var observing = false
    private var choosing = false

    func present() {
        let picker = SCContentSharingPicker.shared
        if !observing {
            picker.add(self)
            observing = true
        }
        var configuration = SCContentSharingPickerConfiguration()
        configuration.allowedPickerModes = [.singleWindow, .singleApplication, .singleDisplay]
        configuration.excludedBundleIDs = Bundle.main.bundleIdentifier.map { [$0] } ?? []
        configuration.allowsChangingSelectedContent = false
        picker.defaultConfiguration = configuration
        picker.maximumStreamCount = 1
        picker.isActive = true
        choosing = true
        picker.present()
    }

    func reset() {
        choosing = false
        if observing {
            SCContentSharingPicker.shared.remove(self)
            SCContentSharingPicker.shared.isActive = false
            observing = false
        }
    }

    nonisolated func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor stream: SCStream?) {
        Task { @MainActor in
            guard choosing, stream == nil else { return }
            choosing = false
            onCancel?()
        }
    }

    nonisolated func contentSharingPicker(_ picker: SCContentSharingPicker, didUpdateWith filter: SCContentFilter, for stream: SCStream?) {
        Task { @MainActor in
            guard choosing, stream == nil else { return }
            choosing = false
            onSelection?(filter, Self.label(for: filter))
        }
    }

    nonisolated func contentSharingPickerStartDidFailWithError(_ error: Error) {
        Task { @MainActor in
            guard choosing else { return }
            choosing = false
            onError?(error.localizedDescription)
        }
    }

    private static func label(for filter: SCContentFilter) -> String {
        if #available(macOS 15.2, *) {
            if filter.style == .application, !filter.includedApplications.isEmpty {
                return "App: " + filter.includedApplications.map(\.applicationName).joined(separator: ", ")
            }
            if filter.style == .window, let window = filter.includedWindows.first {
                let app = window.owningApplication?.applicationName ?? "Window"
                return window.title.flatMap { $0.isEmpty ? nil : "\(app): \($0)" } ?? app
            }
            if filter.style == .display, let display = filter.includedDisplays.first {
                return "Whole screen (display \(display.displayID))"
            }
        }
        switch filter.style {
        case .window: return "Selected window"
        case .application: return "Selected app"
        case .display: return "Whole screen"
        default: return "Selected video source"
        }
    }
}
