#!/bin/bash
# Builds Janus.app for Apple silicon: the SwiftUI app, the bundled `ic` CLI
# (Contents/MacOS/ic) with the tools it runs (a minimal LGPL ffmpeg and Anthropic's `ant`, next to
# it, so users install nothing from Homebrew), ICRecorder.app for `ic record` (Contents/Helpers),
# and Sparkle for in-place updates (Contents/Frameworks). Signs it inside out with the hardened
# runtime. Adapted from MacLink's build-app.sh.
#
#   IC_RELEASE_VERSION    1.2.3 or 1.2.3-preview.N (default: the version in Cargo.toml)
#   IC_CODESIGN_IDENTITY  signing identity (default: this Mac's first "Apple Development" identity,
#                         which keeps privacy permissions across rebuilds)
#   IC_APP_OUTPUT         the .app to produce (default: mac/build/Janus.app)
#   IC_UPDATE_FEED        Sparkle feed URL, or "none". Developer ID (release) builds default to the
#                         public feed; other builds get none and never replace themselves.
#
# No notarization or publishing happens here: see scripts/notarize-release.sh.
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    printf '%s\n' 'Build on an Apple silicon Mac.' >&2
    exit 1
fi
for tool in cargo swift codesign lipo plutil ditto otool; do
    command -v "$tool" >/dev/null || { printf 'Missing build tool: %s\n' "$tool" >&2; exit 1; }
done

version="${IC_RELEASE_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)}"
version="${version#v}"
if [[ ! "$version" =~ ^(0|[1-9][0-9]{0,3})\.(0|[1-9][0-9]?)\.(0|[1-9][0-9]?)(-preview\.([1-9][0-9]{0,2}))?$ ]]; then
    printf 'Invalid release version: %s (use 1.2.3 or 1.2.3-preview.N)\n' "$version" >&2
    exit 1
fi
major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]} preview=${BASH_REMATCH[5]:-999}
# Sparkle compares CFBundleVersion: each preview sorts before its final release, which sorts
# before any later version's previews.
build_number=$(( (major * 10000 + minor * 100 + patch) * 1000 + preview ))
short_version="${version%%-*}"

identity="${IC_CODESIGN_IDENTITY:-$(security find-identity -v -p codesigning | awk -F'"' '/"Apple Development:/ { print $2; exit }')}"
if [[ -z "$identity" ]]; then
    printf '%s\n' 'No signing identity found; set IC_CODESIGN_IDENTITY.' >&2
    exit 1
fi
release_build=false
[[ "$identity" == "Developer ID Application:"* ]] && release_build=true
feed="${IC_UPDATE_FEED:-}"
if [[ -z "$feed" && "$release_build" = true ]]; then
    feed="https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest/download/appcast.xml"
fi

app="${IC_APP_OUTPUT:-$project_root/mac/build/Janus.app}"
[[ "$app" = /* ]] || app="$project_root/$app"
if [[ "$app" != *.app || -L "$app" ]]; then
    printf 'App output must be a non-symlink .app path: %s\n' "$app" >&2
    exit 1
fi

MACOSX_DEPLOYMENT_TARGET=14.4 cargo build --locked --release
sparkle_dir="$("$project_root/scripts/fetch-sparkle.sh")"
ffmpeg_dir="$("$project_root/scripts/fetch-ffmpeg.sh")"
ant_dir="$("$project_root/scripts/fetch-ant.sh")"
(cd mac && swift build -c release)
bin="$(cd mac && swift build -c release --show-bin-path)"

mkdir -p "$(dirname "$app")"
stage="$(mktemp -d "$(dirname "$app")/.ic-build.XXXXXX")"
trap 'rm -rf "$stage"' EXIT
bundle="$stage/Janus.app"
recorder="$bundle/Contents/Helpers/ICRecorder.app"
sparkle="$bundle/Contents/Frameworks/Sparkle.framework"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources" "$bundle/Contents/Frameworks" "$recorder/Contents/MacOS"
cp "$bin/InterviewCoach" "$bundle/Contents/MacOS/InterviewCoach"
cp target/release/ic "$bundle/Contents/MacOS/ic"
cp "$ffmpeg_dir/bin/ffmpeg" "$bundle/Contents/MacOS/ffmpeg"
cp "$ant_dir/ant" "$bundle/Contents/MacOS/ant"
cp "$ffmpeg_dir/LICENSE.txt" "$bundle/Contents/Resources/ffmpeg-LICENSE.txt"
cp "$ffmpeg_dir/BUILD.txt" "$bundle/Contents/Resources/ffmpeg-BUILD.txt"
cp "$ant_dir/LICENSE.txt" "$bundle/Contents/Resources/ant-LICENSE.txt"
cp "$bin/ICRecorder" "$recorder/Contents/MacOS/ICRecorder"
cp mac/Resources/Brand/AppIcon.icns "$bundle/Contents/Resources/AppIcon.icns"
cp mac/Resources/Brand/report.css "$bundle/Contents/Resources/report.css"
cp mac/Resources/Brand/report-brand.html "$bundle/Contents/Resources/report-brand.html"
cp mac/Resources/InterviewCoach-Info.plist "$bundle/Contents/Info.plist"
cp mac/Resources/ICRecorder-Info.plist "$recorder/Contents/Info.plist"

# Sparkle, arm64 only. The app isn't sandboxed, so Sparkle's XPC services are unused, and its
# headers and modules are build-time only.
ditto "$sparkle_dir/Sparkle.framework" "$sparkle"
for unused in XPCServices Headers PrivateHeaders Modules; do
    rm -rf "${sparkle:?}/Versions/B/$unused" "${sparkle:?}/$unused"
done
for binary in "$sparkle/Versions/B/Sparkle" "$sparkle/Versions/B/Autoupdate" "$sparkle/Versions/B/Updater.app/Contents/MacOS/Updater"; do
    lipo -thin arm64 "$binary" -output "$binary.arm64" && mv "$binary.arm64" "$binary"
done
cp "$sparkle_dir/LICENSE" "$bundle/Contents/Resources/Sparkle-LICENSE.txt"

for plist in "$bundle/Contents/Info.plist" "$recorder/Contents/Info.plist"; do
    /usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $short_version" "$plist"
    /usr/libexec/PlistBuddy -c "Set :CFBundleVersion $build_number" "$plist"
done
/usr/libexec/PlistBuddy -c "Set :ICReleaseVersion $version" "$bundle/Contents/Info.plist"
if [[ -n "$feed" && "$feed" != none ]]; then
    /usr/libexec/PlistBuddy -c "Add :SUFeedURL string $feed" "$bundle/Contents/Info.plist"
fi
plutil -lint -s "$bundle/Contents/Info.plist" "$recorder/Contents/Info.plist"

# Notarization requires a secure timestamp; development builds skip that network round trip.
sign_options=(--force --options runtime --sign "$identity")
if [[ "$release_build" = true ]]; then sign_options+=(--timestamp); else sign_options+=(--timestamp=none); fi
entitlements=mac/Resources/ICRecorder.entitlements  # microphone access under the hardened runtime
# Inside out, never --deep: Sparkle's helpers, the framework, the recorder, the CLI, then the app.
codesign "${sign_options[@]}" "$sparkle/Versions/B/Autoupdate"
codesign "${sign_options[@]}" "$sparkle/Versions/B/Updater.app"
codesign "${sign_options[@]}" "$sparkle"
codesign "${sign_options[@]}" --entitlements "$entitlements" "$recorder"
codesign "${sign_options[@]}" "$bundle/Contents/MacOS/ic"
# The bundled tools are signed as ours too (their pinned checksums prove where they came from).
codesign "${sign_options[@]}" "$bundle/Contents/MacOS/ffmpeg"
codesign "${sign_options[@]}" "$bundle/Contents/MacOS/ant"
codesign "${sign_options[@]}" --entitlements "$entitlements" "$bundle"
codesign --verify --deep --strict "$bundle"

for executable in "$bundle/Contents/MacOS/InterviewCoach" "$bundle/Contents/MacOS/ic" \
    "$bundle/Contents/MacOS/ffmpeg" "$bundle/Contents/MacOS/ant" \
    "$recorder/Contents/MacOS/ICRecorder" "$sparkle/Versions/B/Sparkle" "$sparkle/Versions/B/Autoupdate" \
    "$sparkle/Versions/B/Updater.app/Contents/MacOS/Updater"; do
    if [[ "$(lipo -archs "$executable")" != arm64 ]]; then
        printf 'Expected an Apple silicon executable: %s\n' "$executable" >&2
        exit 1
    fi
done
if ! otool -l "$bundle/Contents/MacOS/InterviewCoach" | grep -q '@executable_path/../Frameworks'; then
    printf '%s\n' 'The app is missing its @executable_path/../Frameworks rpath, so Sparkle would not load.' >&2
    exit 1
fi

# Replace the output only with a complete, verified bundle.
[[ ! -e "$app" ]] || mv "$app" "$stage/previous.app"
mv "$bundle" "$app"
printf 'Built %s\n  version %s (build %s), signed by %s\n' "$app" "$version" "$build_number" "$identity"
if [[ -n "$feed" && "$feed" != none ]]; then
    printf '  updates from %s\n' "$feed"
else
    printf '%s\n' '  no update feed (development build: never replaces itself)'
fi
