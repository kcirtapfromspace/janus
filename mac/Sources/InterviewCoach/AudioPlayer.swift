import AVFoundation
import Observation

/// Plays an interview's listening copy (both tracks mixed). The transcript and quoted evidence
/// jump to their moment in it.
@MainActor @Observable
final class AudioPlayer {
    private(set) var path: String?
    private(set) var isPlaying = false
    private(set) var currentTime: Double = 0
    private(set) var duration: Double = 0
    @ObservationIgnored private var player: AVPlayer?
    @ObservationIgnored private var timeObserver: Any?

    var isLoaded: Bool { player != nil }

    func load(_ path: String?) {
        guard path != self.path else { return }
        unload()
        self.path = path
        guard let path else { return }
        let player = AVPlayer(url: URL(fileURLWithPath: path))
        self.player = player
        let interval = CMTime(seconds: 0.25, preferredTimescale: 600)
        timeObserver = player.addPeriodicTimeObserver(forInterval: interval, queue: .main) { [weak self] time in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.currentTime = time.seconds
                self.isPlaying = player.rate > 0
                if let total = player.currentItem?.duration.seconds, total.isFinite { self.duration = total }
            }
        }
    }

    func togglePlay() {
        guard let player else { return }
        if player.rate > 0 { player.pause() } else { player.play() }
        isPlaying = player.rate > 0
    }

    /// Jump to `seconds` and play from there.
    func play(from seconds: Double) {
        guard let player else { return }
        player.seek(to: CMTime(seconds: seconds, preferredTimescale: 600), toleranceBefore: .zero, toleranceAfter: .zero)
        player.play()
        isPlaying = true
    }

    func seek(to seconds: Double) {
        player?.seek(to: CMTime(seconds: seconds, preferredTimescale: 600))
        currentTime = seconds
    }

    private func unload() {
        if let timeObserver { player?.removeTimeObserver(timeObserver) }
        timeObserver = nil
        player?.pause()
        player = nil
        isPlaying = false
        currentTime = 0
        duration = 0
    }
}
