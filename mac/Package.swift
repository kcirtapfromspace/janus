// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "InterviewCoachMac",
    platforms: [.macOS("14.4")],
    targets: [
        // Dual-track capture: system audio via a Core Audio process tap + the mic.
        .target(name: "ICRecorderCore"),
        // Headless recorder that `ic record` launches (CLI flow).
        .executableTarget(name: "ICRecorder", dependencies: ["ICRecorderCore"]),
        // Menu-bar + window app; records in-process and hands sessions to the bundled `ic`.
        .executableTarget(name: "InterviewCoach", dependencies: ["ICRecorderCore"]),
        .testTarget(name: "ICRecorderCoreTests", dependencies: ["ICRecorderCore"]),
    ]
)
