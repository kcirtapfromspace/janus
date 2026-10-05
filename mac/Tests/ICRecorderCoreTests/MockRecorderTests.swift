import AVFoundation
@testable import ICRecorderCore
import XCTest

final class MockRecorderTests: XCTestCase {
    func testTheInterviewerHasAPlainEnglishVoice() throws {
        let voice = try XCTUnwrap(MockRecorder.voice)
        XCTAssertTrue(voice.language.hasPrefix("en"), voice.language)
        XCTAssertFalse(voice.voiceTraits.contains(.isNoveltyVoice))
    }

    /// What the voice track gets: the synthesizer's buffers joined into one mono 24 kHz buffer of
    /// about the right length, with sound in it.
    func testSpeechBecomesOneVoiceTrackBuffer() throws {
        let synthesizer = AVSpeechSynthesizer()
        let utterance = AVSpeechUtterance(string: "Tell me about a project you're proud of.")
        utterance.voice = MockRecorder.voice
        var pieces: [AVAudioPCMBuffer] = []
        let finished = expectation(description: "synthesized")
        synthesizer.write(utterance) { buffer in
            guard let pcm = buffer as? AVAudioPCMBuffer else { return }
            if pcm.frameLength == 0 { finished.fulfill() } else { pieces.append(pcm) }
        }
        wait(for: [finished], timeout: 20)
        XCTAssertFalse(pieces.isEmpty)
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 24000, channels: 1, interleaved: false)!
        let speech = try XCTUnwrap(MockRecorder.convert(pieces, to: format, log: Logger(fileURL: nil)))
        let seconds = Double(speech.frameLength) / 24000
        XCTAssertGreaterThan(seconds, 1.0, "a sentence takes a second or more")
        XCTAssertLessThan(seconds, 8.0)
        let samples = UnsafeBufferPointer(start: speech.floatChannelData![0], count: Int(speech.frameLength))
        XCTAssertGreaterThan(samples.reduce(Float(0)) { max($0, abs($1)) }, 0.01, "not silence")
    }
}
