import InterviewCoachKit
import SwiftUI
import WebKit

/// Session list on the left, the selected interview's stages on the right. The toolbar holds a
/// fixed set of controls that always fit; anything with a message (progress, errors, setup, a live
/// recording problem) goes in a banner under it, where there's room for the whole sentence.
struct MainWindow: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        @Bindable var model = model
        NavigationSplitView {
            LibrarySidebar()
                .navigationSplitViewColumnWidth(min: 240, ideal: 290, max: 400)
        } detail: {
            Group {
                if model.listSelection.count > 1 {
                    BatchView()
                } else if let detail = model.detail, detail.session.id == model.selection {
                    VStack(spacing: 0) {
                        if let session = model.selectedSession, session.isDeleted { DeletedBanner(session: session) }
                        PipelineView(detail: detail)
                    }
                } else if model.selection != nil {
                    ProgressView()
                } else {
                    ContentUnavailableView("Select an interview", systemImage: "doc.text.magnifyingglass")
                }
            }
            .safeAreaInset(edge: .top, spacing: 0) { StatusBanner() }
        }
        .task(id: model.selection) { await model.loadDetail() }
        .toolbar { toolbar }
        .refreshWhenShown { await model.refresh() }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .navigation) {
            recordControl
        }
        ToolbarItemGroup(placement: .primaryAction) {
            let selected = model.selectedSession
            let busy = model.phase.isBusy
            Button("Import", systemImage: "square.and.arrow.down") { model.importRecording() }
                .help("Import a recording (audio or video)")
                .disabled(busy)
            Menu("Outcome", systemImage: "flag") {
                ForEach(outcomeChoices, id: \.value) { choice in
                    Button(choice.label) { if let s = selected { model.setOutcome(s.id, choice.value) } }
                }
            }
            .help("Record how the interview actually turned out")
            .disabled(selected == nil || busy)
            Button("Show in Finder", systemImage: "folder") { if let s = selected { model.revealInFinder(s) } }
                .help("Show this interview's folder in Finder")
                .disabled(selected == nil)
            Button("Setup", systemImage: "gearshape") { openWindow(id: "setup") }
                .help("Set up Interview Coach: Docker, the AI proxy, sign-in, models, keys, recording test")
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
            .fixedSize()
            .help("Stop recording")
        default:
            Button { openWindow(id: "record") } label: {
                Label("Record", systemImage: "record.circle")
                    .foregroundStyle(.red)
            }
            .labelStyle(.titleAndIcon)
            .fixedSize()
            .help("Record an interview from any app (your mic + the call's audio)")
            .disabled(model.phase.isBusy)
        }
    }
}

/// One line under the toolbar for whatever needs saying right now, most urgent first: a live
/// recording problem, an error, what's running, or setup still to finish.
struct StatusBanner: View {
    @Environment(AppModel.self) private var model
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        if let problem = model.captureProblem {
            row(problem, systemImage: "exclamationmark.triangle.fill", tint: .orange)
        } else if let error = model.lastError {
            row(error, systemImage: "xmark.octagon.fill", tint: .red) {
                Button("Dismiss") { model.lastError = nil }
            }
        } else if case .working(let label) = model.phase {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(label).font(.callout).lineLimit(2)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(.bar)
            .overlay(alignment: .bottom) { Divider() }
        } else if let setup = model.setup, !setup.ready {
            row(setup.remaining == 1 ? "Setup has 1 thing left before interviews can be analysed."
                    : "Setup has \(setup.remaining) things left before interviews can be analysed.",
                systemImage: "wrench.and.screwdriver", tint: .orange) {
                Button("Open Setup") { openWindow(id: "setup") }
            }
        }
    }

    private func row(_ text: String, systemImage: String, tint: Color, @ViewBuilder action: () -> some View = { EmptyView() })
        -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Image(systemName: systemImage).foregroundStyle(tint)
            Text(text).font(.callout).lineLimit(3).textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
            action().controlSize(.small).fixedSize()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 8)
        .background(tint.opacity(0.10))
        .overlay(alignment: .bottom) { Divider() }
    }
}

/// Shows report.html, reloading when ic rewrites it (e.g. after setting an outcome). Its timestamp
/// links (`#t=754.0`) hand their moment to `onSeek`; web links open in the browser.
struct ReportView: NSViewRepresentable {
    let path: String
    let onSeek: (Double) -> Void
    /// A link to another version of the report (its analysis id).
    var onOpenReport: (Int) -> Void = { _ in }

    @MainActor final class Coordinator: NSObject, WKNavigationDelegate {
        var loaded: (path: String, modified: Date?)?
        var onSeek: (Double) -> Void
        var onOpenReport: (Int) -> Void

        init(onSeek: @escaping (Double) -> Void, onOpenReport: @escaping (Int) -> Void) {
            self.onSeek = onSeek
            self.onOpenReport = onOpenReport
        }

        /// WebKit asks here about same-page `#t=` clicks too (HTML and SVG links alike), so the report
        /// needs no script. Anything that isn't a timestamp or web link, like the report itself or an
        /// in-page anchor, loads as usual.
        func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                     decisionHandler: @escaping @MainActor (WKNavigationActionPolicy) -> Void) {
            guard let url = action.request.url else { return decisionHandler(.allow) }
            if url.isFileURL, url.path == webView.url?.path,
               let seconds = seekSeconds(fromFragment: url.fragment(percentEncoded: false)) {
                onSeek(seconds)
                decisionHandler(.cancel)
            } else if action.navigationType == .linkActivated, let page = webView.url,
                      let id = reportID(fromLink: url, currentPage: page) {
                onOpenReport(id)
                decisionHandler(.cancel)
            } else if action.navigationType == .linkActivated, ["http", "https"].contains(url.scheme?.lowercased()) {
                NSWorkspace.shared.open(url)
                decisionHandler(.cancel)
            } else {
                decisionHandler(.allow)
            }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(onSeek: onSeek, onOpenReport: onOpenReport) }

    func makeNSView(context: Context) -> WKWebView {
        let view = WKWebView()
        view.setValue(false, forKey: "drawsBackground")  // no white flash in dark mode
        view.navigationDelegate = context.coordinator
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        context.coordinator.onSeek = onSeek
        context.coordinator.onOpenReport = onOpenReport
        let url = URL(fileURLWithPath: path)
        let modified = (try? FileManager.default.attributesOfItem(atPath: path)[.modificationDate]) as? Date
        guard context.coordinator.loaded?.path != path || context.coordinator.loaded?.modified != modified else { return }
        context.coordinator.loaded = (path, modified)
        view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }
}
