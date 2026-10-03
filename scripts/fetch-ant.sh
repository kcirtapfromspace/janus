#!/bin/bash
# Fetches the pinned release of Anthropic's CLI (`ant`, MIT-licensed), which Interview Coach bundles
# for Claude's browser sign-in, so users never install it. Verifies the archive's and the
# license's SHA-256, then prints the directory holding `ant` and LICENSE.txt. Later runs reuse the
# verified copy.
set -euo pipefail
project_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version=1.38.0
checksum=fd3d4de01d66d3a9c4d38c77bda73eadf4012e50f3696dbe8baa7e5e06e3f30a          # ant_${version}_macos_arm64.zip
license_checksum=8bf96984ff8bcfae7e48cae76a529e8a25317ba9e02abf7fd3cc64fdf95657a6  # LICENSE at v$version
directory="$project_root/mac/vendor/ant-$version"
if [[ ! -f "$directory/.verified" ]]; then
    mkdir -p "$project_root/mac/vendor"
    work="$(mktemp -d "$project_root/mac/vendor/ant.XXXXXX")"
    trap 'rm -rf "$work"' EXIT
    curl -fsSL --retry 3 -o "$work/ant.zip" \
        "https://github.com/anthropics/anthropic-cli/releases/download/v$version/ant_${version}_macos_arm64.zip"
    curl -fsSL --retry 3 -o "$work/LICENSE.txt" \
        "https://raw.githubusercontent.com/anthropics/anthropic-cli/v$version/LICENSE"
    if ! printf '%s  %s\n%s  %s\n' "$checksum" "$work/ant.zip" "$license_checksum" "$work/LICENSE.txt" \
        | shasum -a 256 -c - >/dev/null; then
        printf 'ant %s did not match its pinned checksums.\n' "$version" >&2
        exit 1
    fi
    unzip -q -o "$work/ant.zip" ant -d "$work"
    [[ "$(lipo -archs "$work/ant")" = arm64 ]] || { printf '%s\n' 'Expected an arm64 ant.' >&2; exit 1; }
    rm -rf "$directory"
    mkdir -p "$directory"
    mv "$work/ant" "$work/LICENSE.txt" "$directory/"
    touch "$directory/.verified"
fi
printf '%s\n' "$directory"
