import SwiftUI
import WebKit

/// Session list on the left, the selected interview's report on the right, controls in the toolbar.
struct MainWindow: View {
    @Environment(AppModel.self) private var model
    @State private var askingConsent = false

    var body: some View {
        @Bindable var model = model
        NavigationSplitView {
            List(model.sessions, selection: $model.selection) { session in
                SessionRow(session: session).tag(session.id)
            }
            .navigationSplitViewColumnWidth(min: 240, ideal: 290)
            .overlay {
                if model.sessions.isEmpty {
                    ContentUnavailableView("No interviews yet", systemImage: "waveform",
                                           description: Text("Record one with the Record button, or import a recording."))
                }
            }
        } detail: {
            if let session = model.selectedSession {
                SessionDetail(session: session)
            } else {
                ContentUnavailableView("Select an interview", systemImage: "doc.text.magnifyingglass")
            }
        }
        .toolbar { toolbar }
        .confirmationDialog("Has everyone on the call agreed to be recorded?", isPresented: $askingConsent) {
            Button("Yes — start recording") { Task { await model.startRecording() } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Some places require every participant's consent.")
        }
        .task { await model.refresh() }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .navigation) {
            recordControl
        }
        ToolbarItem(placement: .status) {
            statusLine
        }
        ToolbarItemGroup(placement: .primaryAction) {
            let selected = model.selectedSession
            let busy = model.phase.isBusy
            Button("Import", systemImage: "square.and.arrow.down") { model.importRecording() }
                .help("Import a recording (audio or video)")
                .disabled(busy)
            Button("Analyze", systemImage: "sparkles") { if let s = selected { model.analyze(s.id) } }
                .help("Analyse this interview again")
                .disabled(selected == nil || busy || selected?.status == "recording")
            Menu("Outcome", systemImage: "flag") {
                ForEach(outcomeChoices, id: \.value) { choice in
                    Button(choice.label) { if let s = selected { model.setOutcome(s.id, choice.value) } }
                }
            }
            .help("Record how the interview actually turned out")
            .disabled(selected == nil || busy)
            Button("Open in Browser", systemImage: "safari") { if let s = selected { model.openInBrowser(s) } }
                .disabled(selected?.reportPath == nil)
            Button("Show in Finder", systemImage: "folder") { if let s = selected { model.revealInFinder(s) } }
                .disabled(selected == nil)
        }
    }

    @ViewBuilder private var recordControl: some View {
        switch model.phase {
        case .recording(let since):
            Button { model.stopRecording() } label: {
                HStack(spacing: 6) {
                    Image(systemName: "stop.circle.fill").foregroundStyle(.red)
                    ElapsedTime(since: since)
                }
            }
            .help("Stop recording")
        default:
            Button { askingConsent = true } label: {
                Label("Record", systemImage: "record.circle")
                    .foregroundStyle(.red)
            }
            .labelStyle(.titleAndIcon)
            .help("Record an interview from any app (your mic + the call's audio)")
            .disabled(model.phase.isBusy)
        }
    }

    @ViewBuilder private var statusLine: some View {
        if case .working(let label) = model.phase {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text(label).font(.callout)
            }
        } else if let error = model.lastError {
            Label(error, systemImage: "xmark.octagon.fill")
                .font(.callout)
                .foregroundStyle(.red)
                .lineLimit(1)
                .help(error)
        } else if let problem = model.health?.problems.first {
            Label(problem, systemImage: "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(.orange)
                .lineLimit(1)
                .help(problem)
        }
    }
}

struct SessionDetail: View {
    @Environment(AppModel.self) private var model
    let session: SessionSummary

    var body: some View {
        if let report = session.reportPath {
            ReportView(path: report)
        } else {
            ContentUnavailableView {
                Label(heading, systemImage: session.status == "failed" ? "exclamationmark.triangle" : "doc.text")
            } description: {
                Text(session.error ?? detail)
            } actions: {
                if session.status != "recording" {
                    Button("Analyze") { model.analyze(session.id) }
                        .disabled(model.phase.isBusy)
                }
            }
        }
    }

    private var heading: String {
        switch session.status {
        case "failed": "This session failed"
        case "recording": "Recording in progress"
        default: "Not analysed yet"
        }
    }

    private var detail: String {
        session.status == "recording" ? "Stop the recording to transcribe and analyse it."
                                      : "Transcribed, but there's no analysis yet."
    }
}

/// Shows report.html, reloading when ic rewrites it (e.g. after setting an outcome).
struct ReportView: NSViewRepresentable {
    let path: String

    final class Coordinator {
        var loaded: (path: String, modified: Date?)?
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> WKWebView {
        let view = WKWebView()
        view.setValue(false, forKey: "drawsBackground")  // no white flash in dark mode
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        let url = URL(fileURLWithPath: path)
        let modified = (try? FileManager.default.attributesOfItem(atPath: path)[.modificationDate]) as? Date
        guard context.coordinator.loaded?.path != path || context.coordinator.loaded?.modified != modified else { return }
        context.coordinator.loaded = (path, modified)
        view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }
}
