#!/usr/bin/env bash
# Development build of mac/build/Interview Coach.app, signed with your Apple Development identity
# so privacy permissions stick across rebuilds. It has no update feed, so it never replaces itself.
# Releases: scripts/notarize-release.sh (see docs/RELEASING.md).
exec "$(dirname "$0")/../scripts/build-app.sh" "$@"
