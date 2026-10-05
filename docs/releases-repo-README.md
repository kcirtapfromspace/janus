# Janus releases

Notarized builds of [Janus](https://github.com/kcirtapfromspace/interview-coach) for Apple silicon, and the update feed installed copies follow. The source repository is private; this repository holds only the signed releases.

Janus records your job interviews from any app, transcribes them on your Mac, and tells you how each one went and what to work on.

## Install

Needs an Apple silicon Mac with macOS 14.4 or later.

1. Download the latest `InterviewCoach-…-macos-arm64.zip` from [Releases](https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest) and unzip it.
2. **Move Janus.app to Applications before opening it.** Opened from Downloads, macOS runs it from a temporary read-only copy, which can't update itself.
3. Open it. The **Setup** window lists what's left, each with one button:
   - **Sign in to Claude or ChatGPT**: approve access in your browser and choose an available coaching model. ChatGPT plan usage requires account permission; an OpenAI API key selects separate API billing.
   - **Jev evaluation**: add a TypeSafe key for the required answer and interviewer evaluations.
   - **Speech models**: about 1.7 GB, once.
   - **Test recording**: 5 seconds, to check your mic and the call audio are both captured.
4. Optional, for the `ic` command line:
   `ln -s "/Applications/Janus.app/Contents/MacOS/ic" /usr/local/bin/ic`

No Docker or Homebrew setup is needed: ffmpeg (a minimal LGPL build) and Anthropic's `ant` are inside the app.

Janus keeps your existing Interview Coach interviews, preferences, and sign-ins. The notebook includes local analytics and Light, Dark, and System appearance; System is the default.

Installed copies update themselves from this repository's latest release, and only install while nothing is being recorded or analysed.

## Verify a download

Each release lists the archive's SHA-256 in `SHA256SUMS.txt`:

```sh
shasum -a 256 -c SHA256SUMS.txt
```

Each app is signed with Developer ID (team `67C7724279`) and notarized by Apple. Updates are additionally EdDSA-signed, and the app only accepts a signed feed.
