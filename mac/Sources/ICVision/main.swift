import AVFoundation
import Foundation
import Vision

/// Finds the faces in a recorded call's video, on this Mac, with Apple's Vision framework.
///
/// Only measurements leave this tool: where each face is, which way it points, and how open its
/// mouth is, a few times a second. No images, no face templates, nothing that identifies anyone.
/// `ic` turns these into observable cues (who's on camera, nods, looking away); the formulas live
/// there, in src/video.rs.

let usage = """
usage: ICVision faces --video <file> --out <faces.json> [--fps <samples per second>] [--progress]

Writes every face Vision finds, sampled from the video, to <faces.json>. With --progress,
prints {"progress": 0.42} lines to stdout as it goes.
"""

func die(_ message: String, code: Int32 = 2) -> Never {
    FileHandle.standardError.write("ICVision: \(message)\n".data(using: .utf8)!)
    if code == 2 { FileHandle.standardError.write("\n\(usage)\n".data(using: .utf8)!) }
    exit(code)
}

struct Options {
    var video: URL
    var out: URL
    var fps = 6.0
    var progress = false
}

func parse(_ arguments: [String]) -> Options {
    guard arguments.count > 1, arguments[1] == "faces" else { die("expected the `faces` command") }
    var video: String?, out: String?
    var fps = 6.0
    var progress = false
    var index = 2
    func value(_ flag: String) -> String {
        index += 1
        guard index < arguments.count else { die("missing value for \(flag)") }
        return arguments[index]
    }
    while index < arguments.count {
        switch arguments[index] {
        case "--video": video = value("--video")
        case "--out": out = value("--out")
        case "--fps":
            guard let v = Double(value("--fps")), v > 0, v <= 30 else { die("--fps must be between 0 and 30") }
            fps = v
        case "--progress": progress = true
        case "-h", "--help":
            print(usage)
            exit(0)
        default: die("unknown argument \(arguments[index])")
        }
        index += 1
    }
    guard let video, let out else { die("--video and --out are required") }
    return Options(video: URL(fileURLWithPath: video), out: URL(fileURLWithPath: out), fps: fps, progress: progress)
}

/// One face in one sample. Positions are fractions of the frame, from its top-left corner;
/// angles are degrees; `mouth` is the inner lips' opening as a fraction of the face's height.
struct Face: Encodable {
    var x, y, w, h: Double
    var yaw: Double?
    var pitch: Double?
    var roll: Double?
    var mouth: Double?
    var conf: Double
}

struct Sample: Encodable {
    var t: Double
    var faces: [Face]
}

struct Output: Encodable {
    var version = 1
    var tool = "vision-faces-v1"
    var video: String
    var fps: Double
    var width: Int
    var height: Int
    var durationS: Double
    var samples: [Sample]
}

func rounded(_ v: Double, _ places: Double = 10_000) -> Double { (v * places).rounded() / places }
func degrees(_ radians: NSNumber?) -> Double? { radians.map { rounded($0.doubleValue * 180 / .pi, 10) } }

func faces(in pixels: CVPixelBuffer) throws -> [Face] {
    let handler = VNImageRequestHandler(cvPixelBuffer: pixels, orientation: .up)
    let rectangles = VNDetectFaceRectanglesRequest()
    rectangles.revision = VNDetectFaceRectanglesRequestRevision3  // has pitch as well as yaw and roll
    try handler.perform([rectangles])
    let found = rectangles.results ?? []
    guard !found.isEmpty else { return [] }
    let landmarks = VNDetectFaceLandmarksRequest()
    landmarks.inputFaceObservations = found
    try handler.perform([landmarks])
    let marked = landmarks.results ?? []
    return found.enumerated().map { i, face in
        let box = face.boundingBox  // normalized, origin bottom-left
        var mouth: Double?
        if i < marked.count, let lips = marked[i].landmarks?.innerLips, lips.pointCount > 2 {
            let ys = lips.normalizedPoints.map { Double($0.y) }  // fractions of the face box
            mouth = rounded((ys.max() ?? 0) - (ys.min() ?? 0))
        }
        return Face(
            x: rounded(box.minX), y: rounded(1 - box.maxY), w: rounded(box.width), h: rounded(box.height),
            yaw: degrees(face.yaw), pitch: degrees(face.pitch), roll: degrees(face.roll), mouth: mouth,
            conf: rounded(Double(face.confidence), 1000)
        )
    }
}

let options = parse(CommandLine.arguments)
let asset = AVURLAsset(url: options.video)
let track: AVAssetTrack
let duration: Double
do {
    guard let first = try await asset.loadTracks(withMediaType: .video).first else {
        die("\(options.video.path) has no video", code: 1)
    }
    track = first
    duration = try await asset.load(.duration).seconds
} catch {
    die("couldn't open \(options.video.path): \(error.localizedDescription)", code: 1)
}
let size = (try? await track.load(.naturalSize)) ?? .zero

let reader: AVAssetReader
do {
    reader = try AVAssetReader(asset: asset)
} catch {
    die("couldn't read \(options.video.path): \(error.localizedDescription)", code: 1)
}
let output = AVAssetReaderTrackOutput(track: track, outputSettings: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
])
output.alwaysCopiesSampleData = false
reader.add(output)
guard reader.startReading() else {
    die("couldn't decode \(options.video.path): \(reader.error?.localizedDescription ?? "unknown error")", code: 1)
}

var samples: [Sample] = []
let interval = 1 / options.fps
var next = 0.0
var reported = -1.0
while let buffer = output.copyNextSampleBuffer() {
    let t = buffer.presentationTimeStamp.seconds
    guard t.isFinite, t + 1e-6 >= next, let pixels = buffer.imageBuffer else { continue }
    // Sample on a fixed grid; a frame that stood still for a while (the recorder only writes
    // frames that changed) is sampled once, and the next sample waits for the next frame.
    while next <= t + 1e-6 { next += interval }
    do {
        samples.append(Sample(t: rounded(t, 1000), faces: try faces(in: pixels)))
    } catch {
        die("Vision failed at \(t) s: \(error.localizedDescription)", code: 1)
    }
    if options.progress, duration > 0, t / duration - reported >= 0.02 {
        reported = t / duration
        print("{\"progress\": \(rounded(min(reported, 1), 1000))}")
        fflush(stdout)
    }
}
if reader.status == .failed {
    die("decoding stopped: \(reader.error?.localizedDescription ?? "unknown error")", code: 1)
}

let encoder = JSONEncoder()
encoder.keyEncodingStrategy = .convertToSnakeCase
do {
    let data = try encoder.encode(Output(
        video: options.video.lastPathComponent, fps: options.fps, width: Int(size.width), height: Int(size.height),
        durationS: rounded(duration, 1000), samples: samples
    ))
    try data.write(to: options.out, options: .atomic)
} catch {
    die("couldn't write \(options.out.path): \(error.localizedDescription)", code: 1)
}
if options.progress {
    print("{\"progress\": 1}")
}
