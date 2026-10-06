import AppKit
import InterviewCoachKit
import SwiftUI

/// Developer tool for checking layout without screenshots: launched with `IC_SNAPSHOTS=<folder>`
/// (and optionally `IC_SNAPSHOT_DETAIL=<ic session JSON>`), the app draws its own views into
/// offscreen windows at several widths, saves each as a PNG, and quits. Nothing on screen is captured.
@MainActor
enum Snapshots {
    static func runIfRequested(model: AppModel) {
        let env = ProcessInfo.processInfo.environment
        guard let folder = env["IC_SNAPSHOTS"] else { return }
        Task { @MainActor in
            let out = URL(fileURLWithPath: folder, isDirectory: true)
            try? FileManager.default.createDirectory(at: out, withIntermediateDirectories: true)
            model.appearance = .light
            if env["IC_SNAPSHOT_LIBRARY"] == nil && env["IC_SNAPSHOT_DETAIL"] == nil { await model.refresh() }
            if let path = env["IC_SNAPSHOT_DETAIL"], let data = try? Data(contentsOf: URL(fileURLWithPath: path)) {
                model.detail = try? ICClient.decode(SessionDetail.self, from: data)
                model.selection = model.detail?.session.id
            }
            await render(RecordWindow().environment(model), size: CGSize(width: 490, height: 0), name: "record", to: out)
            model.recordVideo = true
            model.screenPermission = true
            model.fallbackToScreen = true
            await render(RecordWindow().environment(model), size: CGSize(width: 490, height: 0), name: "record-screen-fallback", to: out)
            model.fallbackToScreen = false
            model.selectedVideoLabel = "Google Chrome: Microsoft Teams"
            await render(RecordWindow().environment(model), size: CGSize(width: 490, height: 0), name: "record-selected-window", to: out)
            model.selectedVideoLabel = "Whole screen"
            model.selectedVideoIsScreen = true
            await render(RecordWindow().environment(model), size: CGSize(width: 490, height: 0), name: "record-selected-screen", to: out)
            model.resetVideoSource()
            if let detail = model.detail {
                for width in [540.0, 760, 1000] {
                    for stage in StageStep.allCases {
                        model.selectedStage = stage
                        await render(PipelineView(detail: detail).environment(model), size: CGSize(width: width, height: 500),
                                     name: "pipeline-\(Int(width))-\(stage.rawValue)", to: out)
                    }
                }
                model.selectedStage = .report
            }
            if let path = env["IC_SNAPSHOT_LIBRARY"], let data = try? Data(contentsOf: URL(fileURLWithPath: path)),
               let library = try? ICClient.decode(Library.self, from: data) {
                model.library = library
                await render(LibrarySidebar().environment(model), size: CGSize(width: 300, height: 640), name: "sidebar", to: out)
                model.filter.showArchived = true
                await render(LibrarySidebar().environment(model), size: CGSize(width: 300, height: 700), name: "sidebar-archived", to: out)
                model.filter.showArchived = false
                model.listSelection = [1, 2, 4]
                await render(BatchView().environment(model), size: CGSize(width: 520, height: 300), name: "batch", to: out)
                model.listSelection = []
                if let one = library.sessions.first {
                    await render(EditDetailsSheet(session: one).environment(model), size: CGSize(width: 440, height: 0), name: "edit", to: out)
                }
            }
            await render(DashboardView().environment(model), size: CGSize(width: 936, height: 800), name: "dashboard", to: out)
            await render(DashboardView().environment(model), size: CGSize(width: 550, height: 1000), name: "dashboard-narrow", to: out)
            model.appearance = .dark
            await render(DashboardView().environment(model), size: CGSize(width: 936, height: 800), name: "dashboard-dark", to: out)
            model.appearance = .light
            let savedLibrary = model.library
            model.library = Library()
            await render(DashboardView().environment(model), size: CGSize(width: 750, height: 600), name: "dashboard-empty", to: out)
            model.library = savedLibrary
            for width in [820.0, 1200] {
                await renderWindow(MainWindow().environment(model), size: CGSize(width: width, height: 820),
                                   name: "window-\(Int(width))", to: out)
            }
            if let detail = model.detail {
                model.selection = detail.session.id
                model.selectedStage = .next
                await renderWindow(MainWindow().environment(model), size: CGSize(width: 1200, height: 820), name: "window-prepare", to: out)
                model.selectedStage = .transcript
                await renderWindow(MainWindow().environment(model), size: CGSize(width: 820, height: 700), name: "window-transcript-narrow", to: out)
                model.selectedStage = .report
                await renderWindow(MainWindow().environment(model), size: CGSize(width: 1200, height: 820),
                                   name: "window-review", to: out, appearanceChanges: model)
                model.appearance = .light
            }
            // The toolbar in its busier states.
            let states: [(String, AppModel.Phase, String?)] = [
                ("working", .working("Importing 0002-interview-2026-10-02-10-01 system.wav…"), nil),
                ("recording", .recording(since: Date().addingTimeInterval(-754)), nil),
                ("error", .idle, "The coaching provider could not be reached. Your recording is saved. Try the review again when your connection returns."),
            ]
            for (name, phase, error) in states {
                model.phase = phase
                model.lastError = error
                await renderWindow(MainWindow().environment(model), size: CGSize(width: 820, height: 300),
                                   name: "window-820-\(name)", to: out)
            }
            model.phase = .idle
            model.lastError = nil
            await render(SetupView().environment(model), size: CGSize(width: 560, height: 700), name: "setup", to: out)
            model.appearance = .dark
            await render(SetupView().environment(model), size: CGSize(width: 560, height: 700), name: "setup-dark", to: out)
            exit(0)
        }
    }

    /// A view on its own. Height 0 means "as tall as it wants".
    private static func render<V: View>(_ view: V, size: CGSize, name: String, to folder: URL) async {
        let host = NSHostingView(rootView: view.frame(width: size.width).frame(height: size.height > 0 ? size.height : nil)
            .background(Color(nsColor: .windowBackgroundColor)))
        let fitting = size.height > 0 ? size : CGSize(width: size.width, height: host.fittingSize.height)
        let window = NSWindow(contentRect: NSRect(origin: CGPoint(x: -10_000, y: -10_000), size: fitting),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.contentView = host
        window.orderFrontRegardless()
        try? await Task.sleep(for: .milliseconds(400))
        save(host, name: name, to: folder)
        window.orderOut(nil)
    }

    /// A whole window, title bar and toolbar included.
    private static func renderWindow<V: View>(_ view: V, size: CGSize, name: String, to folder: URL,
                                             appearanceChanges model: AppModel? = nil) async {
        let host = NSHostingView(rootView: view)
        host.sceneBridgingOptions = [.toolbars, .title]
        let window = NSWindow(contentRect: NSRect(origin: CGPoint(x: -10_000, y: -10_000), size: size),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.title = "Janus"
        window.toolbarStyle = .unified
        window.contentView = host
        window.orderFrontRegardless()
        try? await Task.sleep(for: .milliseconds(800))
        save(window.contentView?.superview ?? host, name: name, to: folder)
        // Change the real preference while the same window and report remain open.
        if let model {
            for appearance in [AppAppearance.dark, .light, .system] {
                model.appearance = appearance
                precondition(NSApp.appearance?.name == (appearance == .system ? nil : appearance == .dark ? .darkAqua : .aqua))
                try? await Task.sleep(for: .milliseconds(500))
                save(window.contentView?.superview ?? host, name: "\(name)-\(appearance.rawValue)", to: folder)
            }
        }
        window.orderOut(nil)
    }

    private static func save(_ view: NSView, name: String, to folder: URL) {
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: folder.appendingPathComponent("\(name).png"))
    }
}
