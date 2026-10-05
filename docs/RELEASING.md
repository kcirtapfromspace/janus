# Releasing

Releases are built, signed, notarized, and published from this Mac, following MacLink's process. Nothing runs in GitHub Actions.

- **Source:** `kcirtapfromspace/interview-coach` (private). Each version gets a release tagged at its commit, carrying the archive, `SHA256SUMS.txt`, `BUILD-MANIFEST.json`, and the validation transcripts.
- **Downloads and update feed:** `kcirtapfromspace/interview-coach-releases` (public). It holds the notarized archive, the signed `appcast.xml`, and checksums. Installed copies read `releases/latest/download/appcast.xml`. GitHub's "latest" only ever points to a full release, so **feed releases are never marked pre-release**: a pre-release there would silently stop updates for everyone.

## One-time setup (already done on the build Mac)

- **Developer ID Application** certificate and private key in the login Keychain (team `67C7724279`).
- **Notary credentials:** a `notarytool` Keychain profile. The `MacLink` profile works here too, because it belongs to the same Apple account. To make a separate one, run `xcrun notarytool store-credentials InterviewCoach` yourself in Terminal. Never put the secret in chat, source, or GitHub.
- **Sparkle update-signing key:** an EdDSA key in the login Keychain, under Sparkle account `dev.interviewcoach`. The app carries only the public half (`SUPublicEDKey` in `mac/Resources/InterviewCoach-Info.plist`).

### Back up the Sparkle key

It's the only key that can sign updates for installed copies. If it's lost, every installed copy is orphaned and has to be reinstalled by hand. Export it once and keep the file somewhere other than this Mac, such as a password manager:

```sh
"$(scripts/fetch-sparkle.sh)/bin/generate_keys" --account dev.interviewcoach -x interview-coach-sparkle-key.txt
```

To use it on another build Mac, import it there with `generate_keys --account dev.interviewcoach -f FILE`, then delete the exported file.

## Cut a release

1. Write `docs/release-notes-vVERSION.md`.
2. Commit and push.
3. Notarize. This runs `scripts/ci-local.sh` (clippy, unit and end-to-end tests, Swift tests), builds a Developer ID release with the update feed, submits it to Apple, waits, staples, checks it with Gatekeeper, and writes `dist/InterviewCoach-vVERSION-macos-arm64.zip`. The archive now contains `Janus.app`; the archive prefix and signing identifiers retain their existing names:

   ```sh
   export IC_CODESIGN_IDENTITY='Developer ID Application: Patrick Deutsch (67C7724279)'
   export IC_NOTARY_PROFILE=MacLink
   scripts/notarize-release.sh 0.1.0-preview.2
   ```

   If Apple is still processing when the wait ends, run `scripts/notarize-release.sh --resume VERSION`.
4. Review `dist/notarization/vVERSION/notary-log.json`.
5. Publish both releases and confirm the public feed serves the new build. It refuses an archive that isn't stapled, isn't from team `67C7724279`, follows a different feed, or is older than what the feed serves:

   ```sh
   scripts/publish-release.sh 0.1.0-preview.2
   ```

Installed copies pick the update up within about four hours, or straight away with **Check for Updates…** in the menu-bar panel. They install it only while nothing is being recorded or analysed.

## Versions

`VERSION` is `1.2.3` or `1.2.3-preview.N`. `scripts/build-app.sh` derives the numeric `CFBundleVersion` that Sparkle compares as `(major·10000 + minor·100 + patch)·1000 + N`, using 999 for a final release. So every preview sorts before its release, and every release sorts before the next version's previews. The full version string is stored as `ICReleaseVersion`.

Development builds (`mac/build.sh`) are signed with Apple Development and have no feed, so they never replace themselves. Switching between a development build and a release build changes the code signature, so macOS asks again once for Microphone and System Audio Recording.

## Bundled tools

The app carries its own ffmpeg and `ant` next to `ic` in `Contents/MacOS`, so users install nothing from Homebrew. `scripts/build-app.sh` gets both through pinned, checksum-verified scripts and caches them in `mac/vendor/`:

- **`scripts/fetch-ffmpeg.sh`** builds a minimal, audio-only, LGPL ffmpeg from the release tarball. The tarball's signature was checked once against FFmpeg's release key; its SHA-256 is pinned. The first build takes about a minute.
  - It covers every format `ic` reads, including WebM/Opus and video files, whose audio track it extracts.
  - Its license and configure options ship as `Resources/ffmpeg-LICENSE.txt` and `ffmpeg-BUILD.txt`.
- **`scripts/fetch-ant.sh`** fetches Anthropic's MIT-licensed CLI release, with the zip and its license pinned. Its license ships as `Resources/ant-LICENSE.txt`.

Both are re-signed with the app's identity. To upgrade either one, change its version and checksum in the script. Then run `scripts/ci-local.sh`, which runs the end-to-end tests with the bundled ffmpeg.
