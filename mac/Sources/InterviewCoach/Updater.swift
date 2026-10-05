import AppKit
import Sparkle

/// Keeps Janus current from the public release feed. Only release builds carry a
/// SUFeedURL; development builds never replace themselves. Sparkle checks every four hours,
/// downloads in the background, and verifies the signed feed, the update's EdDSA signature, and
/// that it's signed by the same Developer ID. The update then installs and relaunches only while
/// nothing is being recorded or analysed.
@MainActor
final class Updater: NSObject, SPUUpdaterDelegate {
    private var controller: SPUStandardUpdaterController?
    private var installNow: (() -> Void)?
    /// True when relaunching now wouldn't interrupt a recording or an analysis.
    var isIdle: () -> Bool = { true }
    /// Called when a verified update is waiting.
    var onReady: ((String) -> Void)?

    var isAvailable: Bool { controller != nil }

    func start() {
        guard Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") != nil else { return }
        controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: self, userDriverDelegate: nil)
        // A long-running menu-bar app also looks once at launch.
        controller?.updater.checkForUpdatesInBackground()
    }

    /// "Check for Updates…", with Sparkle's own progress window.
    func checkForUpdates() {
        NSApp.activate(ignoringOtherApps: true)
        controller?.checkForUpdates(nil)
    }

    /// Installs a downloaded, verified update — quitting and relaunching — if nothing is busy.
    func installIfIdle() {
        guard let installNow, isIdle() else { return }
        self.installNow = nil
        installNow()
    }

    nonisolated func updater(_ updater: SPUUpdater, willInstallUpdateOnQuit item: SUAppcastItem,
                             immediateInstallationBlock immediateInstallHandler: @escaping () -> Void) -> Bool {
        let version = item.displayVersionString
        // Sparkle calls its delegate on the main thread.
        MainActor.assumeIsolated {
            installNow = immediateInstallHandler
            onReady?(version)
            installIfIdle()
        }
        return true
    }
}
