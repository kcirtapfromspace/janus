#!/bin/bash
# Build a release locally, have Apple's notary service verify it, staple the ticket, and package
#   dist/InterviewCoach-vVERSION-macos-arm64.zip (+ .sha256)
# plus the validation transcripts the GitHub release carries. Adapted from MacLink's
# notarize-release.sh: no GitHub Actions, no credentials in source, no Gatekeeper overrides.
#
#   IC_CODESIGN_IDENTITY  'Developer ID Application: NAME (TEAM)'
#   IC_NOTARY_PROFILE     an existing `xcrun notarytool` Keychain profile name (e.g. MacLink)
#
#   scripts/notarize-release.sh VERSION            build, submit, wait, staple, package
#   scripts/notarize-release.sh --resume VERSION   continue a submission Apple was still processing
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

resume=false
if [[ "${1:-}" = "--resume" ]]; then
    resume=true
    shift
fi
version="${1:-}"
version="${version#v}"
if [[ $# != 1 || ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-preview\.[1-9][0-9]*)?$ ]]; then
    printf '%s\n' 'Usage: scripts/notarize-release.sh [--resume] VERSION   (e.g. 0.1.0-preview.1)' >&2
    exit 1
fi
profile="${IC_NOTARY_PROFILE:-}"
if [[ -z "$profile" ]]; then
    printf '%s\n' 'Set IC_NOTARY_PROFILE to an existing notarytool Keychain profile name.' >&2
    printf '%s\n' 'First-time setup (run it yourself in Terminal): xcrun notarytool store-credentials InterviewCoach' >&2
    exit 1
fi

state="$project_root/dist/notarization/v$version"
evidence="$project_root/dist/release/v$version"
bundle="$state/Janus.app"
submission="$state/submission.json"
input_archive="$state/submitted.zip"
mkdir -p "$state" "$evidence"
if ! mkdir "$state/workflow.lock" 2>/dev/null; then
    printf 'Locked at %s/workflow.lock; check for another running notarization first.\n' "$state" >&2
    exit 1
fi
verification=""
cleanup() {
    [[ -z "$verification" ]] || rm -rf "$verification"
    rmdir "$state/workflow.lock"
}
trap cleanup EXIT

if [[ "$resume" = true ]]; then
    if [[ ! -f "$submission" || ! -f "$input_archive" ]]; then
        printf '%s\n' 'No saved submission for this version; inspect `xcrun notarytool history` before resubmitting.' >&2
        exit 1
    fi
    (cd "$state" && shasum -a 256 -c submitted.sha256)
else
    identity="${IC_CODESIGN_IDENTITY:-}"
    if [[ "$identity" != "Developer ID Application:"* ]]; then
        printf '%s\n' "Set IC_CODESIGN_IDENTITY to your 'Developer ID Application: …' identity." >&2
        exit 1
    fi
    if [[ -e "$submission" || -e "$state/submission-pending.json" ]]; then
        printf '%s\n' 'This version was already submitted. Resume it, or inspect Apple history before submitting again.' >&2
        exit 1
    fi
    # Checks the named credential works without printing or reading its secret.
    xcrun notarytool history --keychain-profile "$profile" --output-format json >/dev/null
    "$project_root/scripts/ci-local.sh" | tee "$evidence/local-validation.txt"
    IC_CODESIGN_IDENTITY="$identity" IC_RELEASE_VERSION="$version" IC_APP_OUTPUT="$bundle" \
        "$project_root/scripts/build-app.sh"
    codesign -dv --verbose=4 "$bundle" 2> "$state/signature.txt"
    if ! grep -q '^Authority=Developer ID Application:' "$state/signature.txt"; then
        printf '%s\n' 'The app must be signed with Developer ID Application before submission.' >&2
        exit 1
    fi
    COPYFILE_DISABLE=1 /usr/bin/ditto -c -k --norsrc --noextattr --noqtn --keepParent "$bundle" "$input_archive"
    (cd "$state" && shasum -a 256 submitted.zip > submitted.sha256)
    # Keep an ambiguous failed attempt visible; never silently resubmit.
    xcrun notarytool submit "$input_archive" --keychain-profile "$profile" \
        --no-wait --output-format json > "$state/submission-pending.json"
    mv "$state/submission-pending.json" "$submission"
fi

submission_id="$(plutil -extract id raw -o - "$submission")"
printf 'Apple notarization submission: %s\n' "$submission_id"
xcrun notarytool wait "$submission_id" --keychain-profile "$profile" \
    --timeout 15m --output-format json > "$state/result.json" || true
status="$(plutil -extract status raw -o - "$state/result.json" 2>/dev/null || true)"
if [[ "$status" != Accepted && "$status" != Invalid && "$status" != Rejected ]]; then
    printf 'Apple is still processing. Resume with: scripts/notarize-release.sh --resume %s\n' "$version" >&2
    exit 1
fi
xcrun notarytool log "$submission_id" --keychain-profile "$profile" "$state/notary-log.json"
if [[ "$status" != Accepted ]]; then
    printf 'Apple returned %s. See %s/notary-log.json. Nothing was packaged.\n' "$status" "$state" >&2
    exit 1
fi

# Rebuild the archive from exactly what Apple accepted, never from a mutable build.
(cd "$state" && shasum -a 256 -c submitted.sha256)
verification="$(mktemp -d "$state/verify.XXXXXX")"
/usr/bin/ditto -x -k "$input_archive" "$verification"
app="$verification/Janus.app"
[[ -d "$app" ]] || app="$verification/Interview Coach.app"  # resume an earlier submission
app_name="$(basename "$app")"
xcrun stapler staple "$app"

archive_name="InterviewCoach-v$version-macos-arm64.zip"
staged_archive="$state/notarized.zip"
COPYFILE_DISABLE=1 /usr/bin/ditto -c -k --norsrc --noextattr --noqtn --keepParent "$app" "$staged_archive"
final="$verification/final"
/usr/bin/ditto -x -k "$staged_archive" "$final"
{
    printf 'Janus %s notarization\nApple submission %s: %s\n\n' "$version" "$submission_id" "$status"
    printf '%s\n' '== stapler validate (extracted release archive)'
    xcrun stapler validate "$final/$app_name" 2>&1
    printf '\n%s\n' '== codesign --verify --deep --strict'
    codesign --verify --deep --strict --verbose=2 "$final/$app_name" 2>&1
    printf '\n%s\n' '== Gatekeeper (spctl --assess --type execute)'
    spctl --assess --type execute --verbose=2 "$final/$app_name" 2>&1
    printf '\n%s\n' '== signature'
    codesign -dv --verbose=2 "$final/$app_name" 2>&1 | grep -E '^(Identifier|Authority|TeamIdentifier|Timestamp|Runtime Version)='
} | tee "$evidence/notarization-validation.txt"
mv "$staged_archive" "$project_root/dist/$archive_name"
(cd "$project_root/dist" && shasum -a 256 "$archive_name" > "$archive_name.sha256")
printf '\nNotarized release ready: dist/%s\n' "$archive_name"
printf 'Review any Apple warnings in %s/notary-log.json, then: scripts/publish-release.sh %s\n' "$state" "$version"
