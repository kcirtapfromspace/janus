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
            await model.refresh()
            if let path = env["IC_SNAPSHOT_DETAIL"], let data = try? Data(contentsOf: URL(fileURLWithPath: path)) {
                model.detail = try? ICClient.decode(SessionDetail.self, from: data)
                model.selection = model.detail?.session.id
            }
            await render(RecordWindow().environment(model), size: CGSize(width: 440, height: 0), name: "record", to: out)
            if let detail = model.detail {
                for width in [540.0, 760, 1000] {
                    for stage in StageStep.allCases {
                        model.selectedStage = stage
                        await render(PipelineView(detail: detail).environment(model), size: CGSize(width: width, height: 360),
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
            for width in [820.0, 1100] {
                await renderWindow(MainWindow().environment(model), size: CGSize(width: width, height: 560),
                                   name: "window-\(Int(width))", to: out)
            }
            // The toolbar in its busier states.
            let states: [(String, AppModel.Phase, String?)] = [
                ("working", .working("Importing 0002-interview-2026-10-02-10-01 system.wav…"), nil),
                ("recording", .recording(since: Date().addingTimeInterval(-754)), nil),
                ("error", .idle, "Couldn't reach the AI proxy: connection refused. Is Docker running? Open Setup to start it."),
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
    private static func renderWindow<V: View>(_ view: V, size: CGSize, name: String, to folder: URL) async {
        let host = NSHostingView(rootView: view)
        host.sceneBridgingOptions = [.toolbars, .title]
        let window = NSWindow(contentRect: NSRect(origin: CGPoint(x: -10_000, y: -10_000), size: size),
                              styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
                              backing: .buffered, defer: false)
        window.title = "Interview Coach"
        window.toolbarStyle = .unified
        window.contentView = host
        window.orderFrontRegardless()
        try? await Task.sleep(for: .milliseconds(800))
        save(window.contentView?.superview ?? host, name: name, to: folder)
        window.orderOut(nil)
    }

    private static func save(_ view: NSView, name: String, to folder: URL) {
        guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return }
        view.cacheDisplay(in: view.bounds, to: rep)
        try? rep.representation(using: .png, properties: [:])?.write(to: folder.appendingPathComponent("\(name).png"))
    }
}
