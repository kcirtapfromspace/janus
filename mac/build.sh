#!/usr/bin/env bash
# Builds and signs both macOS apps into mac/build/:
#   Interview Coach.app  menu-bar + window app; records in-process, bundles the `ic` CLI
#   ICRecorder.app       headless recorder that `ic record` launches (CLI flow)
#
# Both must be signed bundles whose Info.plist carries NSAudioCaptureUsageDescription: without it
# macOS hands the process tap all-zero buffers instead of failing. Signing with a stable identity
# (not ad hoc) keeps the privacy permissions granted across rebuilds.
#
# Override the identity with CODESIGN_IDENTITY="<name or SHA-1>".
set -euo pipefail
cd "$(dirname "$0")"

IDENTITY="${CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk '/"Apple Development:/ { print $2; exit }')}"
if [[ -z "$IDENTITY" ]]; then
  echo "No 'Apple Development' signing identity found; set CODESIGN_IDENTITY." >&2
  exit 1
fi
sign() {  # sign <path> [entitlements]
  codesign --force --options runtime --timestamp=none ${2:+--entitlements "$2"} --sign "$IDENTITY" "$1"
}

# Always rebuild ic so the copy bundled in the app is never stale.
cargo build --release --manifest-path ../Cargo.toml
swift build -c release
BIN="$(swift build -c release --show-bin-path)"

# Bundles are overwritten in place; codesign --force replaces old signatures.
APP=build/ICRecorder.app
mkdir -p "$APP/Contents/MacOS"
cp -f "$BIN/ICRecorder" "$APP/Contents/MacOS/ICRecorder"
cp -f Resources/ICRecorder-Info.plist "$APP/Contents/Info.plist"
plutil -lint -s "$APP/Contents/Info.plist"
sign "$APP" Resources/ICRecorder.entitlements
codesign --verify --deep --strict "$APP"

APP="build/Interview Coach.app"
mkdir -p "$APP/Contents/MacOS"
cp -f "$BIN/InterviewCoach" "$APP/Contents/MacOS/InterviewCoach"
cp -f ../target/release/ic "$APP/Contents/MacOS/ic"
cp -f Resources/InterviewCoach-Info.plist "$APP/Contents/Info.plist"
plutil -lint -s "$APP/Contents/Info.plist"
sign "$APP/Contents/MacOS/ic"                          # inner binary first, then the bundle
sign "$APP" Resources/ICRecorder.entitlements          # same entitlement: microphone access
codesign --verify --deep --strict "$APP"

echo "Built $(pwd)/build/Interview Coach.app and $(pwd)/build/ICRecorder.app"
