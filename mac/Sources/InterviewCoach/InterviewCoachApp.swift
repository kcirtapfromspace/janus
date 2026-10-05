import SwiftUI

/// Janus: record from the menu bar during any call, review reports in the window.
@main
struct InterviewCoachApp: App {
    @State private var model: AppModel

    init() {
        let model = AppModel()
        _model = State(initialValue: model)
        Snapshots.runIfRequested(model: model)  // developer tool; does nothing unless IC_SNAPSHOTS is set
    }

    var body: some Scene {
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

        Window("Janus Settings", id: "setup") {
            SetupView()
                .environment(model)
                .tint(CoachTheme.accent)
        }
        .windowResizability(.contentSize)

        Window("Janus", id: "main") {
            MainWindow()
                .environment(model)
                .tint(CoachTheme.accent)
                .frame(minWidth: 820, minHeight: 520)
        }
        .defaultSize(width: 1200, height: 820)
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
            .onChange(of: model.setup) { _, setup in
                guard !model.setupPromptShown, let setup else { return }
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
