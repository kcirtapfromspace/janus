// swift-tools-version:5.10
import Foundation
import PackageDescription

// Sparkle (in-place updates) comes from the pinned, checksum-verified release that
// scripts/fetch-sparkle.sh unpacks here; scripts/build-app.sh runs it first.
let sparkle = URL(fileURLWithPath: #filePath).deletingLastPathComponent().appendingPathComponent("vendor/sparkle-2.10.0").path

let package = Package(
    name: "InterviewCoachMac",
    platforms: [.macOS("14.4")],
    targets: [
        // Dual-track capture: system audio via a Core Audio process tap + the mic.
        .target(name: "ICRecorderCore"),
        // Headless recorder that `ic record` launches (CLI flow); shipped inside the app's Helpers.
        .executableTarget(name: "ICRecorder", dependencies: ["ICRecorderCore"]),
        // The app's model of `ic`: running it, and the JSON it returns. Kept free of Sparkle so it's testable.
        .target(name: "InterviewCoachKit"),
        // Menu-bar + window app; records in-process and hands sessions to the bundled `ic`.
        .executableTarget(
            name: "InterviewCoach",
            dependencies: ["ICRecorderCore", "InterviewCoachKit"],
            swiftSettings: [.unsafeFlags(["-F", sparkle])],
            linkerSettings: [.unsafeFlags([
                "-F", sparkle, "-framework", "Sparkle",
                "-Xlinker", "-rpath", "-Xlinker", "@executable_path/../Frameworks",
            ])]
        ),
        .testTarget(name: "ICRecorderCoreTests", dependencies: ["ICRecorderCore"]),
        .testTarget(name: "InterviewCoachKitTests", dependencies: ["InterviewCoachKit"], resources: [.copy("Fixtures")]),
    ]
)
