#!/bin/bash
# Publishes a notarized release (run after scripts/notarize-release.sh):
#   scripts/publish-release.sh VERSION
#
# One release in Janus contains the notarized archive, signed appcast, checksums, and validation
# evidence. It is marked --latest and never pre-release: installed copies follow releases/latest,
# which GitHub resolves only to full releases.
#
# Only a stapled, Gatekeeper-accepted archive signed by team 67C7724279, whose own feed URL is the
# public feed, is published, and the feed never moves back to an older build.
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"
version="${1:-}"; version="${version#v}"
if [[ $# != 1 || ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-preview\.[1-9][0-9]*)?$ ]]; then
    printf '%s\n' 'Usage: scripts/publish-release.sh VERSION' >&2
    exit 1
fi
release_repo="${IC_RELEASE_REPO:-kcirtapfromspace/janus}"
team=67C7724279
name="InterviewCoach-v$version-macos-arm64.zip"
archive="$project_root/dist/$name"
notes="$project_root/docs/release-notes-v$version.md"
evidence="$project_root/dist/release/v$version"
feed="https://github.com/$release_repo/releases/latest/download/appcast.xml"
for f in "$archive" "$notes" "$evidence/local-validation.txt" "$evidence/notarization-validation.txt"; do
    [[ -f "$f" ]] || { printf 'Missing %s\n' "$f" >&2; exit 1; }
done

# The release tags this commit, so it must be committed and pushed.
git diff --quiet HEAD -- || { printf '%s\n' 'Commit your changes first: the release tags this commit.' >&2; exit 1; }
commit="$(git rev-parse HEAD)"
git fetch -q origin
git merge-base --is-ancestor "$commit" origin/main || { printf '%s\n' 'Push this commit to origin/main first.' >&2; exit 1; }

check="$(mktemp -d "$evidence/.check.XXXXXX")"
trap 'rm -rf "$check"' EXIT
/usr/bin/ditto -x -k "$archive" "$check"
app="$check/Janus.app"
[[ -d "$app" ]] || app="$check/Interview Coach.app"
xcrun stapler validate -q "$app"
spctl --assess --type execute "$app"
codesign --verify --deep --strict "$app"
codesign -dv --verbose=2 "$app" 2> "$check/signature.txt"
grep -q "^TeamIdentifier=$team$" "$check/signature.txt" || { printf 'Not signed by team %s.\n' "$team" >&2; exit 1; }
value() { /usr/libexec/PlistBuddy -c "Print :$1" "$app/Contents/Info.plist"; }
[[ "$(value ICReleaseVersion)" = "$version" ]] || { printf '%s\n' 'The archive is a different release.' >&2; exit 1; }
[[ "$(value SUFeedURL)" = "$feed" ]] || { printf 'The archive follows %s, not %s.\n' "$(value SUFeedURL)" "$feed" >&2; exit 1; }
build="$(value CFBundleVersion)"
current="$(curl -fsSL "$feed" 2>/dev/null | sed -n 's:.*<sparkle\:version>\([0-9][0-9]*\)</sparkle\:version>.*:\1:p' | head -1 || true)"
if [[ -n "$current" ]] && (( current >= build )); then
    printf 'The feed already serves build %s; this archive is build %s.\n' "$current" "$build" >&2
    exit 1
fi

"$project_root/scripts/make-appcast.sh" "$archive" "https://github.com/$release_repo/releases/download/v$version/$name" "$evidence"
cp "$archive" "$evidence/$name"
(cd "$evidence" && shasum -a 256 "$name" > SHA256SUMS.txt)
cat > "$evidence/BUILD-MANIFEST.json" <<JSON
{
  "app": "Janus",
  "version": "$version",
  "build": $build,
  "commit": "$commit",
  "archive": "$name",
  "sha256": "$(shasum -a 256 "$archive" | cut -d' ' -f1)",
  "team": "$team",
  "minimum_macos": "$(value LSMinimumSystemVersion)",
  "architectures": ["arm64"],
  "update_feed": "$feed",
  "published_at": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
}
JSON

gh release create "v$version" --repo "$release_repo" --target "$commit" --latest \
    --title "Janus $version" --notes-file "$notes" \
    "$evidence/$name" "$evidence/appcast.xml" "$evidence/SHA256SUMS.txt" \
    "$evidence/BUILD-MANIFEST.json" "$evidence/local-validation.txt" "$evidence/notarization-validation.txt"

# Confirm what installed copies will read. GitHub's releases/latest redirect is cached briefly, so
# allow it up to two minutes to move to the new release.
served=""
for _ in $(seq 1 12); do
    served="$(curl -fsSL "$feed" || true)"
    grep -q "<sparkle:version>$build</sparkle:version>" <<<"$served" && break
    sleep 10
done
grep -q "<sparkle:version>$build</sparkle:version>" <<<"$served" \
    || { printf '%s\n' 'The public feed still does not serve this build after two minutes.' >&2; exit 1; }
printf 'Published Janus %s.\n  Download: https://github.com/%s/releases/tag/v%s\n  Feed: %s\n' \
    "$version" "$release_repo" "$version" "$feed"
