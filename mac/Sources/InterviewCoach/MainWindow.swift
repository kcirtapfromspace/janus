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
                .navigationSplitViewColumnWidth(min: 240, ideal: 264, max: 360)
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
                    if let error = model.detailError {
                        ContentUnavailableView {
                            Label("Couldn’t open this interview", systemImage: "doc.text")
                        } description: {
                            Text(error)
                        } actions: {
                            Button("Try again") {
                                model.lastError = nil
                                Task { await model.loadDetail() }
                            }
                        }
                    } else {
                        VStack(spacing: 12) {
                            ProgressView()
                            Text("Opening interview…").font(.callout).foregroundStyle(CoachTheme.muted)
                        }
                    }
                } else {
                    DashboardView()
                }
            }
            .background(CoachTheme.canvas)
            .safeAreaInset(edge: .top, spacing: 0) { StatusBanner() }
        }
        .task(id: model.selection) { await model.loadDetail() }
        .tint(CoachTheme.accent)
        .toolbar { toolbar }
        .refreshWhenShown { await model.refresh() }
    }

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        ToolbarItem(placement: .navigation) {
            if model.selection != nil || model.listSelection.count > 1 {
                Button { model.listSelection = [] } label: {
                    Label("Notebook", systemImage: "arrow.left")
                }
                .help("Back to notebook")
            } else {
                Text("Notebook").font(.system(size: 13, weight: .medium))
                    .foregroundStyle(CoachTheme.muted)
            }
        }
        ToolbarItemGroup(placement: .primaryAction) {
            Button("Import", systemImage: "square.and.arrow.down") { model.importRecording() }
                .help("Import an audio or video recording")
                .disabled(model.phase.isBusy)
            recordControl
            Menu {
                if let selected = model.selectedSession {
                    Button("Show interview in Finder", systemImage: "folder") { model.revealInFinder(selected) }
                    Divider()
                }
                AppearanceMenu()
                Button("Settings & audio check", systemImage: "slider.horizontal.3") { openWindow(id: "setup") }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .help("Janus settings and files")
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
                Label("Record interview", systemImage: "record.circle")
                    .foregroundStyle(CoachTheme.accent)
            }
            .labelStyle(.titleAndIcon)
            .fixedSize()
            .help("Record an interview from any app (your mic + the call's audio)")
            .disabled(model.phase.isBusy)
            Button { openWindow(id: "mock") } label: { Label("Practice", systemImage: "person.wave.2") }
                .labelStyle(.titleAndIcon)
                .fixedSize()
                .help("A mock interview: real questions from your past interviews, asked out loud, then reviewed")
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
    @Environment(\.colorScheme) private var colorScheme
    let path: String
    let onSeek: (Double) -> Void
    /// A link to another version of the report (its analysis id).
    var onOpenReport: (Int) -> Void = { _ in }
    /// A "Not what you saw?" link: correct the video cues of the answer starting then.
    var onFix: (Double) -> Void = { _ in }

    @MainActor final class Coordinator: NSObject, WKNavigationDelegate {
        var loaded: (path: String, modified: Date?)?
        var onSeek: (Double) -> Void
        var onOpenReport: (Int) -> Void
        var onFix: (Double) -> Void
        var theme = "light"

        init(onSeek: @escaping (Double) -> Void, onOpenReport: @escaping (Int) -> Void, onFix: @escaping (Double) -> Void) {
            self.onSeek = onSeek
            self.onOpenReport = onOpenReport
            self.onFix = onFix
        }

        func applyTheme(to webView: WKWebView) {
            webView.evaluateJavaScript("document.documentElement.dataset.theme = '\(theme)';", completionHandler: nil)
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            // Use the latest mode even if it changed while this report was loading.
            applyTheme(to: webView)
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
            } else if url.isFileURL, url.path == webView.url?.path,
                      let start = fixSeconds(fromFragment: url.fragment(percentEncoded: false)) {
                onFix(start)
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

    func makeCoordinator() -> Coordinator { Coordinator(onSeek: onSeek, onOpenReport: onOpenReport, onFix: onFix) }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        let theme = colorScheme == .dark ? "dark" : "light"
        context.coordinator.theme = theme
        configuration.userContentController.addUserScript(WKUserScript(
            source: "document.documentElement.dataset.theme = '\(theme)';",
            injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        // Apply the current identity to saved reports too, without rewriting their content.
        if let url = Bundle.main.url(forResource: "report", withExtension: "css"),
           let css = try? String(contentsOf: url, encoding: .utf8),
           let brandURL = Bundle.main.url(forResource: "report-brand", withExtension: "html"),
           let brand = try? String(contentsOf: brandURL, encoding: .utf8),
           let data = try? JSONSerialization.data(withJSONObject: [css, brand]),
           let literal = String(data: data, encoding: .utf8) {
            let source = """
            (() => {
                const style = document.createElement('style');
                style.textContent = \(literal)[0];
                document.head.appendChild(style);
                const main = document.querySelector('main');
                if (main) {
                    const stamp = document.createElement('template');
                    stamp.innerHTML = \(literal)[1];
                    const current = main.querySelector('#janus-brand');
                    if (current) current.replaceWith(stamp.content.cloneNode(true));
                    else main.prepend(stamp.content.cloneNode(true));
                }
            })();
            """
            configuration.userContentController.addUserScript(
                WKUserScript(source: source, injectionTime: .atDocumentEnd, forMainFrameOnly: true))
        }
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.setValue(false, forKey: "drawsBackground")  // no white flash in dark mode
        view.navigationDelegate = context.coordinator
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        context.coordinator.onSeek = onSeek
        context.coordinator.onOpenReport = onOpenReport
        let theme = colorScheme == .dark ? "dark" : "light"
        view.appearance = NSAppearance(named: colorScheme == .dark ? .darkAqua : .aqua)
        if context.coordinator.theme != theme {
            context.coordinator.theme = theme
            context.coordinator.applyTheme(to: view)
        }
        let url = URL(fileURLWithPath: path)
        let modified = (try? FileManager.default.attributesOfItem(atPath: path)[.modificationDate]) as? Date
        guard context.coordinator.loaded?.path != path || context.coordinator.loaded?.modified != modified else { return }
        context.coordinator.loaded = (path, modified)
        view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
    }
}
