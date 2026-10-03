# Interview Coach releases

Notarized builds of [Interview Coach](https://github.com/kcirtapfromspace/interview-coach) for Apple silicon, and the update feed installed copies follow. The source repository is private; this repository holds only the signed releases.

Interview Coach records your job interviews from any app, transcribes them on your Mac, and tells you how each one went and what to work on.

## Install

Needs an Apple silicon Mac with macOS 14.4 or later.

1. Download the latest `InterviewCoach-…-macos-arm64.zip` from [Releases](https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest) and unzip it.
2. **Move Interview Coach.app to Applications before opening it.** Opened from Downloads, macOS runs it from a temporary read-only copy, which can't update itself.
3. Open it. The **Setup** window lists what's left, each with one button:
   - **Docker Desktop**, the one thing you install yourself (it runs the local AI proxy). The app starts it when it's needed.
   - **The AI proxy**: downloaded and started for you.
   - **Sign in to Claude**: approve access in your browser. No API key.
   - **Speech models**: about 1.7 GB, once.
   - **Test recording**: 5 seconds, to check your mic and the call audio are both captured.
4. Optional, for the `ic` command line:
   `ln -s "/Applications/Interview Coach.app/Contents/MacOS/ic" /usr/local/bin/ic`

Nothing comes from Homebrew: ffmpeg (a minimal LGPL build) and Anthropic's `ant` are inside the app.

Installed copies update themselves from this repository's latest release, and only install while nothing is being recorded or analysed.

## Verify a download

Each release lists the archive's SHA-256 in `SHA256SUMS.txt`:

```sh
shasum -a 256 -c SHA256SUMS.txt
```

Each app is signed with Developer ID (team `67C7724279`) and notarized by Apple. Updates are additionally EdDSA-signed, and the app only accepts a signed feed.
