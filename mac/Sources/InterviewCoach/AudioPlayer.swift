import AVFoundation
import AVKit
import Observation
import SwiftUI

/// Plays an interview's listening copy (both tracks mixed), with the call's video when it was
/// recorded. The video file is silent and starts when the audio does, so the two are put together
/// here rather than copied into another file. The transcript and quoted evidence jump to their
/// moment in it.
@MainActor @Observable
final class AudioPlayer {
    private(set) var path: String?
    private(set) var videoPath: String?
    /// Whether the call's video is loaded and showing.
    private(set) var hasVideo = false
    private(set) var isPlaying = false
    private(set) var currentTime: Double = 0
    private(set) var duration: Double = 0
    @ObservationIgnored private(set) var player: AVPlayer?
    @ObservationIgnored private var timeObserver: Any?

    var isLoaded: Bool { player != nil }

    func load(_ path: String?, video: String? = nil) {
        guard path != self.path || video != videoPath else { return }
        unload()
        self.path = path
        videoPath = video
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
        guard let video else { return }
        // The audio plays straight away; the picture joins it once the two files are put together.
        Task { [weak self] in
            guard let both = await Self.audioWithVideo(audio: path, video: video) else { return }
            guard let self, self.player === player, self.videoPath == video else { return }
            let at = player.currentTime()
            player.replaceCurrentItem(with: AVPlayerItem(asset: both))
            if at.seconds > 0 { await player.seek(to: at, toleranceBefore: .zero, toleranceAfter: .zero) }
            self.hasVideo = true
        }
    }

    /// The listening copy's sound with the video's picture, both from 0.
    private nonisolated static func audioWithVideo(audio: String, video: String) async -> AVComposition? {
        let sound = AVURLAsset(url: URL(fileURLWithPath: audio))
        let picture = AVURLAsset(url: URL(fileURLWithPath: video))
        do {
            guard let soundTrack = try await sound.loadTracks(withMediaType: .audio).first,
                  let pictureTrack = try await picture.loadTracks(withMediaType: .video).first else { return nil }
            let (soundLength, pictureLength) = (try await sound.load(.duration), try await picture.load(.duration))
            let both = AVMutableComposition()
            try both.addMutableTrack(withMediaType: .audio, preferredTrackID: kCMPersistentTrackID_Invalid)?
                .insertTimeRange(CMTimeRange(start: .zero, duration: soundLength), of: soundTrack, at: .zero)
            try both.addMutableTrack(withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid)?
                .insertTimeRange(CMTimeRange(start: .zero, duration: pictureLength), of: pictureTrack, at: .zero)
            return both.copy() as? AVComposition
        } catch {
            return nil  // a damaged video: the audio still plays
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
        hasVideo = false
        isPlaying = false
        currentTime = 0
        duration = 0
    }
}

/// The call's video, driven by the player (its controls are the player bar's).
struct CallVideoView: NSViewRepresentable {
    let player: AVPlayer

    func makeNSView(context: Context) -> AVPlayerView {
        let view = AVPlayerView()
        view.controlsStyle = .none
        view.videoGravity = .resizeAspect
        view.player = player
        return view
    }

    func updateNSView(_ view: AVPlayerView, context: Context) {
        if view.player !== player { view.player = player }
    }
}
