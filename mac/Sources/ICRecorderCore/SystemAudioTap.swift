import AVFoundation
import AudioToolbox
import CoreAudio
import Foundation

/// Records a mono mixdown of the audio every other process plays (Zoom, Teams, a browser tab…).
///
/// A Core Audio process tap (macOS 14.2+) is attached to a private aggregate device clocked by the
/// default output device, following Apple's "Capturing system audio with Core Audio taps".
/// Needs `NSAudioCaptureUsageDescription` in Info.plist. Without the permission macOS delivers
/// all-zero buffers instead of an error, so callers must check the track's peak afterwards.
public final class SystemAudioTap {
    /// Diagnostics for recorder.json. Written during prepare() and the first IO callback;
    /// read only after stop() has drained the IO queue.
    public private(set) var details: [String: String] = [:]

    private let log: Logger
    private let queue = DispatchQueue(label: "com.thinkstudio.interviewcoach.recorder.system", qos: .userInitiated)
    private var tapID = AudioObjectID(kAudioObjectUnknown)
    private var aggregateID = AudioObjectID(kAudioObjectUnknown)
    private var ioProcID: AudioDeviceIOProcID?
    private var writer: TrackWriter?
    private var t0Seconds = 0.0
    private var sawFirstBuffer = false
    private var mixScratch: [Float] = []

    public init(log: Logger) {
        self.log = log
    }

    deinit { stop() }

    /// Creates the tap and aggregate device. Returns the sample rate buffers will arrive at.
    public func prepare() throws -> Double {
        let excluded = ownProcessObject().map { [$0] } ?? []
        let description = CATapDescription(monoGlobalTapButExcludeProcesses: excluded)
        description.name = "ICRecorder system audio"
        description.isPrivate = true
        description.muteBehavior = .unmuted

        var tap = AudioObjectID(kAudioObjectUnknown)
        try check(AudioHardwareCreateProcessTap(description, &tap), "AudioHardwareCreateProcessTap")
        tapID = tap

        let tapFormat: AudioStreamBasicDescription = try getProperty(tapID, kAudioTapPropertyFormat)
        guard tapFormat.mFormatFlags & kAudioFormatFlagIsFloat != 0, tapFormat.mBitsPerChannel == 32 else {
            throw CaptureError.message("unexpected tap format (flags \(tapFormat.mFormatFlags), \(tapFormat.mBitsPerChannel) bits); expected 32-bit float")
        }

        let outputDevice: AudioObjectID = try getProperty(AudioObjectID(kAudioObjectSystemObject), kAudioHardwarePropertyDefaultOutputDevice)
        let outputUID = try getStringProperty(outputDevice, kAudioDevicePropertyDeviceUID)
        let outputName = (try? getStringProperty(outputDevice, kAudioObjectPropertyName)) ?? "unknown"

        let aggregate: [String: Any] = [
            kAudioAggregateDeviceNameKey: "ICRecorder Tap",
            kAudioAggregateDeviceUIDKey: UUID().uuidString,
            kAudioAggregateDeviceMainSubDeviceKey: outputUID,
            kAudioAggregateDeviceIsPrivateKey: true,
            kAudioAggregateDeviceIsStackedKey: false,
            kAudioAggregateDeviceTapAutoStartKey: true,
            kAudioAggregateDeviceSubDeviceListKey: [[kAudioSubDeviceUIDKey: outputUID]],
            kAudioAggregateDeviceTapListKey: [[
                kAudioSubTapDriftCompensationKey: true,
                kAudioSubTapUIDKey: description.uuid.uuidString,
            ]],
        ]
        var device = AudioObjectID(kAudioObjectUnknown)
        try check(AudioHardwareCreateAggregateDevice(aggregate as CFDictionary, &device), "AudioHardwareCreateAggregateDevice")
        aggregateID = device

        let rate: Float64 = try getProperty(aggregateID, kAudioDevicePropertyNominalSampleRate)
        details = [
            "output_device": outputName,
            "output_device_uid": outputUID,
            "tap_sample_rate": String(tapFormat.mSampleRate),
            "tap_channels": String(tapFormat.mChannelsPerFrame),
            "aggregate_sample_rate": String(rate),
            "excluded_own_process": excluded.isEmpty ? "no" : "yes",
        ]
        if abs(rate - tapFormat.mSampleRate) > 0.5 {
            log.warn("system: tap reports \(tapFormat.mSampleRate) Hz but the aggregate runs at \(rate) Hz; writing at \(rate) Hz")
        }
        log.info("system: tap ready via output device '\(outputName)' at \(rate) Hz")
        return rate
    }

    /// Starts delivery into `writer`. The first-ever start is where macOS shows the
    /// "System Audio Recording" permission prompt.
    public func start(writer: TrackWriter, t0HostTime: UInt64) throws {
        self.writer = writer
        t0Seconds = AVAudioTime.seconds(forHostTime: t0HostTime)
        var procID: AudioDeviceIOProcID?
        try check(AudioDeviceCreateIOProcIDWithBlock(&procID, aggregateID, queue) { [weak self] _, inputData, inputTime, _, _ in
            self?.process(inputData, inputTime)
        }, "AudioDeviceCreateIOProcIDWithBlock")
        ioProcID = procID
        try check(AudioDeviceStart(aggregateID, procID), "AudioDeviceStart")
    }

    /// Stops capture and tears down the aggregate device and tap. Idempotent.
    public func stop() {
        if let procID = ioProcID {
            let status = AudioDeviceStop(aggregateID, procID)
            if status != noErr { log.warn("system: AudioDeviceStop returned \(fourCC(UInt32(bitPattern: status)))") }
            AudioDeviceDestroyIOProcID(aggregateID, procID)
            ioProcID = nil
        }
        queue.sync {}  // let any in-flight callback finish before the writer is finalized
        if aggregateID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyAggregateDevice(aggregateID)
            aggregateID = AudioObjectID(kAudioObjectUnknown)
        }
        if tapID != AudioObjectID(kAudioObjectUnknown) {
            AudioHardwareDestroyProcessTap(tapID)
            tapID = AudioObjectID(kAudioObjectUnknown)
        }
    }

    // MARK: - IO

    private func process(_ input: UnsafePointer<AudioBufferList>, _ time: UnsafePointer<AudioTimeStamp>) {
        guard let writer else { return }
        let buffers = UnsafeMutableAudioBufferListPointer(UnsafeMutablePointer(mutating: input))
        // The aggregate lists sub-device input streams (e.g. a headset mic, if the output device
        // has one) before its taps, so the tap is the last buffer.
        guard let buffer = buffers.last, let data = buffer.mData, buffer.mDataByteSize > 0 else { return }
        let channels = Int(max(buffer.mNumberChannels, 1))
        let frames = Int(buffer.mDataByteSize) / (MemoryLayout<Float>.size * channels)
        if !sawFirstBuffer {
            sawFirstBuffer = true
            details["io_buffer_count"] = String(buffers.count)
            details["io_tap_buffer_channels"] = String(channels)
            log.info("system: first buffer: \(buffers.count) buffer(s) in list, tap buffer has \(channels) channel(s), \(frames) frames")
        }

        let timestamp = time.pointee
        let hostTime: UInt64? = timestamp.mFlags.contains(.hostTimeValid) ? timestamp.mHostTime : nil
        let start = hostTime.map { AVAudioTime.seconds(forHostTime: $0) - t0Seconds }
        let samples = data.assumingMemoryBound(to: Float.self)

        if channels == 1 {
            writer.append(UnsafeBufferPointer(start: samples, count: frames), startSeconds: start, hostTime: hostTime)
            return
        }
        if mixScratch.count < frames { mixScratch = [Float](repeating: 0, count: frames) }
        let scale = 1 / Float(channels)
        for frame in 0..<frames {
            var sum: Float = 0
            for channel in 0..<channels { sum += samples[frame * channels + channel] }
            mixScratch[frame] = sum * scale
        }
        mixScratch.withUnsafeBufferPointer { mixed in
            writer.append(UnsafeBufferPointer(rebasing: mixed[0..<frames]), startSeconds: start, hostTime: hostTime)
        }
    }

    // MARK: - Core Audio helpers

    private func ownProcessObject() -> AudioObjectID? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyTranslatePIDToProcessObject,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain
        )
        var pid = ProcessInfo.processInfo.processIdentifier
        var object = AudioObjectID(kAudioObjectUnknown)
        var size = UInt32(MemoryLayout<AudioObjectID>.size)
        let status = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address,
            UInt32(MemoryLayout<pid_t>.size), &pid, &size, &object
        )
        return status == noErr && object != AudioObjectID(kAudioObjectUnknown) ? object : nil
    }
}

func getProperty<T>(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector) throws -> T {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var size = UInt32(MemoryLayout<T>.size)
    let value = UnsafeMutableRawPointer.allocate(byteCount: MemoryLayout<T>.size, alignment: MemoryLayout<T>.alignment)
    defer { value.deallocate() }
    try check(AudioObjectGetPropertyData(object, &address, 0, nil, &size, value), "reading \(fourCC(selector)) of object \(object)")
    return value.load(as: T.self)
}

func getStringProperty(_ object: AudioObjectID, _ selector: AudioObjectPropertySelector) throws -> String {
    var address = AudioObjectPropertyAddress(
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain
    )
    var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
    var value: Unmanaged<CFString>?
    try check(AudioObjectGetPropertyData(object, &address, 0, nil, &size, &value), "reading \(fourCC(selector)) of object \(object)")
    guard let value else { throw CaptureError.message("\(fourCC(selector)) of object \(object) is empty") }
    return value.takeRetainedValue() as String
}
