import SwiftUI

/// Interview Coach: record from the menu bar during any call, review reports in the window.
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
        } label: {
            MenuBarLabel().environment(model)
        }
        .menuBarExtraStyle(.menu)

        Window("Record an Interview", id: "record") {
            RecordWindow()
                .environment(model)
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("Set Up Interview Coach", id: "setup") {
            SetupView()
                .environment(model)
        }
        .windowResizability(.contentSize)

        Window("Interview Coach", id: "main") {
            MainWindow()
                .environment(model)
                .frame(minWidth: 820, minHeight: 520)
        }
        .defaultSize(width: 1100, height: 740)
    }
}

/// The menu-bar icon. It's the one view that exists from launch, so it also opens Setup the
/// first time the app starts on a Mac that isn't ready yet.
struct MenuBarLabel: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(systemName: model.captureProblem == nil ? model.phase.menuBarSymbol : "exclamationmark.triangle.fill")
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
