# Mac apps

This Swift package builds two apps with `mac/build.sh` (which also rebuilds the `ic` CLI first):

- **Interview Coach.app** (`Sources/InterviewCoach`): SwiftUI menu-bar + window app. It records in-process with `ICRecorderCore`, and runs the bundled `ic` (`Contents/MacOS/ic`) for everything else: `ic recording begin|finish`, `ic list --json`, `ic doctor --json`, `ic analyze`, `ic outcome`, `ic import`.
- **ICRecorder.app** (`Sources/ICRecorder`): the headless recorder `ic record` launches from the terminal. Described below.

# ICRecorder

A small macOS agent app (no Dock icon or windows) that records an interview as two
time-aligned mono WAVs:

- `system.wav`: everything other apps play (Zoom, Teams, Meet in a browser, …), captured with a
  Core Audio process tap. This is the interviewer.
- `mic.wav`: the default microphone, captured with AVAudioEngine. This is you.

Because the two sides are on separate tracks, "you vs. interviewer" labels need no speaker
detection. Requires macOS 14.4+ on Apple silicon.

## Build

```sh
mac/build.sh          # → mac/build/ICRecorder.app (signed)
cd recorder && swift test  # unit tests for the WAV writer; they don't touch audio devices
```

`build.sh` signs with the first "Apple Development" identity in your keychain, with the hardened
runtime and the `com.apple.security.device.audio-input` entitlement (without that entitlement,
the hardened runtime blocks the mic). Override the identity with `CODESIGN_IDENTITY=…`.
Signing with a stable identity keeps privacy permissions granted across rebuilds; ad-hoc
signatures change on every build and would re-prompt.

It has to be a real, signed `.app`. `NSAudioCaptureUsageDescription` in `Info.plist` is what
lets macOS grant system-audio capture. Without it, or without the permission, **macOS
delivers all-zero buffers instead of an error**.

## Launch

```sh
open -n -a mac/build/ICRecorder.app --args --session-dir /abs/path/to/session [--duration 60] [--aec]
```

Launching with `open` makes ICRecorder itself the app macOS asks for permission. Running
`ICRecorder.app/Contents/MacOS/ICRecorder` directly from Python or a terminal would make macOS
attribute the permission to the terminal/IDE instead, which is fragile.

The first run shows two prompts: Microphone (before capture starts) and System Audio Recording
(at the first tap start). Do a short `--duration 10` test run first so the real interview isn't
the one that waits on a prompt. If the system track comes back silent, check
System Settings > Privacy & Security > Screen & System Audio Recording > "System Audio Recording Only".

## CLI contract

| Argument | Meaning |
|---|---|
| `--session-dir <dir>` | Required, absolute path (apps launched via `open` start in `/`). Created if missing. |
| `--duration <sec>` | Optional; stop automatically after this many seconds. |
| `--aec` | Enable Apple voice processing (echo cancellation) on the mic, so speaker bleed of the interviewer is suppressed. Other-app ducking is set to minimum. Falls back to the raw mic, with a warning, if voice processing won't start. |

Files in `<dir>`:

| File | When |
|---|---|
| `recorder.pid` | Written at start; removed on exit. Its presence means "recording". |
| `system.wav`, `mic.wav` | 16-bit PCM mono at each device's native rate (usually 48 kHz). Header refreshed every second, so a crash still leaves a playable file. |
| `recorder.log` | Human-readable log (stdout isn't visible under `open`). |
| `recorder.json` | Written on exit: see below. |

**Stopping:** send `SIGINT` or `SIGTERM` to the pid in `recorder.pid`, send a quit Apple event
(`osascript -e 'quit app "ICRecorder"'`), or let `--duration` expire. Each path finalizes both
WAVs, writes `recorder.json`, removes the pid file, and exits 0.

**Exit codes:** 0 = stopped normally; 1 = failed to start (the reason is in `recorder.json`
`errors`); 2 = bad arguments, or the directory is already in use or holds an earlier recording
(nothing is written except the log). Under `open` there is no exit code to read, so callers
should wait for `recorder.pid` to disappear and then read `recorder.json`.

The recorder refuses to start if `system.wav`/`mic.wav` already exist in the directory, or if
another live recorder owns `recorder.pid`.

## Alignment: both files start at t0

The recorder takes one host-clock timestamp, t0, just before starting the two captures. Each
track is written so that **sample 0 is t0**:

- the first buffer is preceded by zeros covering the time from t0 to its first sample (audio
  from before t0 is dropped);
- if a buffer arrives more than 50 ms later than the file position implies (dropped or late
  buffers, or a device switch), the gap is filled with zeros, so the file keeps tracking the host
  clock instead of drifting early.

Downstream code can therefore treat second N in `mic.wav` and second N in `system.wav` as the
same instant, with no offsets to apply. The raw first-buffer host times and offsets are still
recorded in `recorder.json` for diagnosis. When a track runs *ahead* of the host clock, the
recorder only reports it (`max_ahead_seconds`) and never drops audio to correct it.

## recorder.json

```json
{
  "version": 1,
  "alignment": "t0_padded",
  "session_dir": "/…/session", "started_at": "…", "stopped_at": "…",
  "stop_reason": "SIGINT | SIGTERM | quit | duration | error",
  "requested_duration_seconds": null, "t0_host_time": 123456789,
  "tracks": {
    "system": {
      "file": "system.wav", "sample_rate": 48000, "channels": 1,
      "frames": 2880000, "duration_seconds": 60.0, "captured_frames": 2879488,
      "first_buffer_host_time": 123456999, "start_offset_seconds": 0.0107,
      "leading_pad_frames": 512, "dropped_leading_frames": 0,
      "gap_fill_frames": 0, "gap_fill_events": 0, "max_ahead_seconds": 0.0,
      "rms": 0.031, "peak": 0.62, "write_error": null,
      "details": { "output_device": "…", "aggregate_sample_rate": "48000.0", "io_buffer_count": "1" }
    },
    "mic": { "…": "same fields; details include device, aec_requested, aec_active" }
  },
  "warnings": [{ "code": "system_silent", "message": "…" }],
  "errors": []
}
```

Warning codes: `system_silent` / `mic_silent` (every sample is exactly zero; for `system` this
is the missing-permission signature), `*_near_silent` (RMS below 1e-4), `*_no_audio` (no buffers
at all), and `mic_warning` (e.g. AEC fallback). Error codes: `mic_permission_denied`,
`start_failed`, `mic_error`, `*_write_error`.

## Layout

```
Package.swift
Sources/ICRecorder/main.swift              argument parsing, NSApplication (accessory), signals
Sources/ICRecorderCore/RecordingSession.swift  lifecycle, pid file, recorder.json, silence checks
Sources/ICRecorderCore/SystemAudioTap.swift    process tap + private aggregate device
Sources/ICRecorderCore/MicCapture.swift        AVAudioEngine mic, optional voice processing
Sources/ICRecorderCore/TrackWriter.swift       t0-aligned mono WAV writer
Resources/Info.plist, Resources/ICRecorder.entitlements
Tests/ICRecorderCoreTests/                      TrackWriter tests
```

Written from scratch. The tap setup follows the aggregate-device pattern from Apple's
"Capturing system audio with Core Audio taps"; no code was copied from AudioTee or SystemAudioKit.
