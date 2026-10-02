# Interview Coach releases

Notarized builds of [Interview Coach](https://github.com/kcirtapfromspace/interview-coach) for Apple silicon, and the update feed installed copies follow. The source repository is private; this repository holds only the signed releases.

Interview Coach records your job interviews from any app, transcribes them on your Mac, and tells you how each one went and what to work on.

## Install

Needs an Apple silicon Mac with macOS 14.4 or later.

1. Install **Docker Desktop** (docker.com) and open it once.
2. In Terminal: `brew install ffmpeg anthropics/tap/ant`
3. Download the latest `InterviewCoach-…-macos-arm64.zip` from [Releases](https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest) and unzip it.
4. **Move Interview Coach.app to Applications before opening it.** Opened from Downloads, macOS runs it from a temporary read-only copy, which can't update itself.
5. Open it. A waveform icon appears in the menu bar. Click it, then **Sign in to Claude…** and approve access in your browser.
6. Optional, for the `ic` command line:
   `ln -s "/Applications/Interview Coach.app/Contents/MacOS/ic" /opt/homebrew/bin/ic`

Installed copies update themselves from this repository's latest release, and only install while nothing is being recorded or analysed.

## Verify a download

Each release lists the archive's SHA-256 in `SHA256SUMS.txt`:

```sh
shasum -a 256 -c SHA256SUMS.txt
```

Each app is signed with Developer ID (team `67C7724279`) and notarized by Apple. Updates are additionally EdDSA-signed, and the app only accepts a signed feed.
