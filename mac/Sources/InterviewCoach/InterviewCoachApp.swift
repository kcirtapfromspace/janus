import SwiftUI

/// Interview Coach: record from the menu bar during any call, review reports in the window.
@main
struct InterviewCoachApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        MenuBarExtra {
            MenuBarView()
                .environment(model)
        } label: {
            Image(systemName: model.captureProblem == nil ? model.phase.menuBarSymbol : "exclamationmark.triangle.fill")
        }
        .menuBarExtraStyle(.window)

        Window("Interview Coach", id: "main") {
            MainWindow()
                .environment(model)
                .frame(minWidth: 820, minHeight: 520)
        }
        .defaultSize(width: 1100, height: 740)
    }
}

extension AppModel.Phase {
    var menuBarSymbol: String {
        switch self {
        case .idle, .confirmingConsent: "waveform"
        case .recording: "record.circle.fill"
        case .working: "hourglass"
        }
    }
}
