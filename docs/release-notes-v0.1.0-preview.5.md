# Interview Coach 0.1.0 preview 5 — A native menu, and analysis that finishes even if you quit

## A native menu-bar menu

The menu-bar icon now opens a standard macOS menu, like Docker Desktop's:

- **Status line at the top:** a coloured dot and what's happening now:
  - Ready to record;
  - Recording since 9:41 PM;
  - what's running;
  - setup still to finish;
  - your mic isn't being recorded.
- **Record Interview… (⌘R)** opens a small window for an optional title and company, and asks you to confirm everyone agreed to be recorded. During a recording, the same item becomes **Stop Recording (⌘.)**.
- **Open Interview Coach (⌘O)** and **Import Recording… (⌘I)**.
- **Recent Interviews** submenu: your last 8 interviews, with their verdicts.
- **Setup… (⌘,)** shows how many things are left. Also **Check for Updates…**, **About**, and **Quit (⌘Q)**.

The menu is rebuilt each time it opens. The app re-reads your interviews every 10 seconds, and setup every minute, so nothing in it goes stale. Before, the panel could show an interview as "Transcribing" long after it had finished. The main window's Record button opens the same Record window.

## Analysis finishes even if the app quits

If the app quit while a stage was running, `ic` stopped at its next message. An import could finish transcribing and then never run its report. Now `ic` drops output nobody is reading and carries on, so the work completes and lands in the database.

## Validation

- **Tests:** 87 Rust unit tests, 17 analysis and stage tests, and 31 Swift tests pass, and clippy is clean.
- **Closed output:** `ic` ran with its output closed mid-run. Preview 4's `ic` exits with an error (code 101); this one finishes (exit 0).
- **Record window:** checked with the offscreen renderer.
- **The menu itself:** not checked by script. macOS doesn't expose an open SwiftUI menu to accessibility tools.
