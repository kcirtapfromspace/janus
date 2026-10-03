# Interview Coach 0.1.0 preview 3 — Set up in the app, nothing to install from Homebrew

Setting up Interview Coach no longer involves Terminal or Homebrew. A **Setup** window lists what's left, each step with one button, and the app does the work.

## Setup window

It opens by itself the first time, and again whenever something is missing. You can also open it any time with **Setup…** in the menu-bar panel. Each row shows what's done, what's next and live progress:

- **Docker Desktop.** It runs the local AI proxy, and it's the only thing you install yourself. If it's installed but not running, the app starts it.
- **AI proxy.** Downloaded (about 2 GB, once) and started for you. Every AI request still goes through it, on this Mac only. If Docker or the proxy has stopped, for example after a restart, the app starts them again before analysing instead of failing.
- **Sign in to Claude, without Terminal.** Approve access in your browser. If the page shows a code instead, paste it into the row.
- **Speech models.** About 1.7 GB, downloaded once with a progress bar, before your first interview rather than in the middle of it. Downloads:
  - resume after an interruption;
  - check free disk space first;
  - are checked against pinned checksums.
- **Microphone permission**, with a button to allow it.
- **Test recording.** Records 5 seconds while playing a short sound, then confirms both your mic and the call audio were captured. macOS can't report whether System Audio Recording is allowed; when it isn't, the call audio is silently recorded as silence. This test catches that before an interview depends on it. Try it with the headphones you use for calls.
- **Optional keys.** OpenAI, and TypeSafe for Jev. Pasted into a secure field and stored only in the AI proxy's settings on this Mac.

## Nothing from Homebrew

- **ffmpeg** (a minimal, audio-only LGPL build) and **Anthropic's `ant`** (for Claude sign-in) now ship inside the app. If you installed them with Homebrew for preview 1, Interview Coach no longer needs them.
- **The bundled ffmpeg** reads everything the old one did: WAV, M4A/MP4/MOV (including video files), MP3, FLAC, AIFF, CAF, Ogg, and browser WebM/Opus recordings.
- **Docker** is found wherever Docker Desktop or OrbStack put it.

## Command line

- `ic setup status`: what's left.
- `ic setup run all`: starts Docker, sets up the AI proxy, and downloads the models.
- `ic proxy key openai|typesafe`: adds a key.
- `ic doctor` shows the same checks.

## Validation

- **Tests:** 66 Rust unit tests, 16 analysis and stage tests, the 3 end-to-end Whisper tests, 27 Swift tests, and clippy pass. The end-to-end tests ran with only the bundled ffmpeg and Homebrew removed from PATH. New tests cover:
  - every setup state (fresh Mac, Docker stopped, signed out, models missing, shared proxy);
  - resumed, corrupted and complete downloads, against a local test server;
  - Docker's image-pull progress;
  - sign-in output;
  - the Setup window's JSON and event stream;
  - the recording test's verdict.
- **Formats:** the bundled ffmpeg handled every input format in the list above, plus HE-AAC.
- **Bare environment:** proxy setup ran with only the system PATH and Docker Desktop's own CLI.
- **Setup window:** launched with an empty data folder, it opened by itself and showed the real status: 2 things left, with the proxy and model rows offering their buttons.
- **Not tested:**
  - a Mac that has never had Homebrew (no fresh macOS VM was available);
  - the sign-in and recording-test buttons, which need you at the Mac.

## Known limits

- The recorder fix from preview 2 (switching headphones mid-call) is still waiting on a real-call test.
- Docker Desktop's own first-run prompts (accepting its terms, allowing its helper) still need a click in Docker.
