#!/bin/bash
# Every local check a release needs. Prints a transcript; scripts/notarize-release.sh saves it as
# the release's local-validation.txt. Runs entirely on this Mac.
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$project_root"

dirty=""
git diff --quiet HEAD -- || dirty=" (+ uncommitted changes)"
printf 'Janus local validation\n%s, commit %s%s, %s\n\n' \
    "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$(git rev-parse --short HEAD)" "$dirty" "$(sw_vers -productVersion | sed 's/^/macOS /')"

printf '%s\n' '== cargo clippy (warnings are errors)'
cargo clippy --locked --all-targets -- -D warnings
printf '\n%s\n' '== cargo test'
cargo test --locked 2>&1 | grep -E '^(test |running|test result)'
printf '\n%s\n' '== end to end: Whisper + speaker detection on the synthetic interviews, with the bundled ffmpeg'
IC_FFMPEG="$("$project_root/scripts/fetch-ffmpeg.sh")/bin/ffmpeg" \
    cargo test --locked --release --test pipeline -- --ignored --test-threads=1 2>&1 | grep -E '^(test |test result)'
printf '\n%s\n' '== swift test (mac/)'
"$project_root/scripts/fetch-sparkle.sh" >/dev/null
(cd mac && swift test 2>&1 | grep -E "Executed|error:|failed" | tail -3)
printf '\n%s\n' 'All local checks passed.'
