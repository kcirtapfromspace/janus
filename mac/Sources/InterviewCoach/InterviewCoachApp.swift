import AppKit
import SwiftUI

/// Janus: record from the menu bar during any call, review reports in the window.
@main
struct InterviewCoachApp: App {
    @State private var model: AppModel
    @NSApplicationDelegateAdaptor(JanusAppDelegate.self) private var appDelegate

    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        appDelegate.model = model
        Snapshots.runIfRequested(model: model)  // developer tool; does nothing unless IC_SNAPSHOTS is set
    }

    var body: some Scene {
        // The first window scene is the app's default launch window.
        Window("Janus", id: "main") {
            MainWindow()
                .environment(model)
                .tint(CoachTheme.accent)
                .frame(minWidth: 820, minHeight: 520)
        }
        .defaultSize(width: 1200, height: 820)
        .commands {
            CommandGroup(replacing: .appInfo) {
                Button("About Janus") { AboutJanus.show() }
            }
            CommandGroup(replacing: .appTermination) {
                Button("Quit Janus") { NSApp.terminate(nil) }
                    .keyboardShortcut("q")
                    .disabled(model.phase.isRecording)
            }
        }

        MenuBarExtra {
            MenuBarMenu()
                .environment(model)
                .tint(CoachTheme.accent)
        } label: {
            MenuBarLabel().environment(model)
                .tint(CoachTheme.accent)
        }
        .menuBarExtraStyle(.menu)

        Window("Record an Interview", id: "record") {
            RecordWindow()
                .environment(model)
                .tint(CoachTheme.accent)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("Practice Interview", id: "mock") {
            MockInterviewWindow()
                .environment(model)
                .tint(CoachTheme.accent)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("What Janus Shares", id: "privacy") {
            PrivacyNoticeWindow()
                .environment(model)
                .tint(CoachTheme.accent)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("Janus Settings", id: "setup") {
            SetupView()
                .environment(model)
                .tint(CoachTheme.accent)
        }
        .windowResizability(.contentSize)
    }
}

/// App switching restores the notebook; Dock and system quit requests honor the recording guard.
@MainActor
final class JanusAppDelegate: NSObject, NSApplicationDelegate {
    static let openNotebook = Notification.Name("JanusOpenNotebook")
    weak var model: AppModel?

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        guard !flag else { return true }
        NotificationCenter.default.post(name: Self.openNotebook, object: nil)
        return false
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        if !NSApp.windows.contains(where: { $0.isVisible && $0.canBecomeKey }) {
            NotificationCenter.default.post(name: Self.openNotebook, object: nil)
        }
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        model?.phase.isRecording == true ? .terminateCancel : .terminateNow
    }
}

/// The menu-bar icon. It's the one view that exists from launch, so it also opens Setup the
/// first time the app starts on a Mac that isn't ready yet.
struct MenuBarLabel: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Group {
            if model.captureProblem != nil {
                Image(systemName: "exclamationmark.triangle.fill")
            } else if model.phase == .idle {
                Image(nsImage: JanusMark.menuBarImage)
            } else {
                Image(systemName: model.phase.menuBarSymbol)
            }
        }
            .accessibilityLabel("Janus")
            .help("Janus")
            .onReceive(NotificationCenter.default.publisher(for: JanusAppDelegate.openNotebook)) { _ in
                openWindow(id: "main")
            }
            .onChange(of: model.setup) { _, setup in
                guard let setup else { return }
                if !model.privacyNoticeShown, setup.privacy?.noticeSeen == false {
                    model.privacyNoticeShown = true
                    NSApp.activate(ignoringOtherApps: true)
                    openWindow(id: "privacy")
                }
                guard !model.setupPromptShown else { return }
                model.setupPromptShown = true
                if !setup.ready {
                    NSApp.activate(ignoringOtherApps: true)
                    openWindow(id: "setup")
                }
            }
    }
}

extension AppModel.Phase {
    var menuBarSymbol: String {
        switch self {
        case .idle: "waveform"
        case .recording: "record.circle.fill"
        case .working: "hourglass"
        }
    }
}
