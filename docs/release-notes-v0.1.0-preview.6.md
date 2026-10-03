# Interview Coach 0.1.0 preview 6 — How the room felt

## A timeline of how the interviewer reacted

Every new report has a section called **How the room felt**, after Interviewer signals.

### The chart

- **Dots:** each thing the interviewer said is a dot. Warmer turns sit higher and cooler ones lower. A turn shows in colour only when it's clearly warm or cool.
- **Line:** the overall trend.
- **Shading:** your answers.
- **Markers:** ★ next steps, $ selling the role, and ! pushback.

### Below the chart

- **The biggest shift,** for example "The room warmed most after your answer at 00:28:43". If nothing changed much, it says so.
- **The warmest and coolest moments,** each quoted with what made it so: "warm tone", "followed up on what you said", "pushed back", "louder than their usual", and so on. A turn is listed only when it has at least one of these cues.
- **Every mention of next steps.**
- **Your voice:** for each answer that stood out, whether you were faster, slower, flatter, more animated, quieter or louder than your usual.

### Click to play

In the app, clicking any dot, shaded answer or timestamp anywhere in the report plays the recording from that moment. The player now sits above the report. Space plays and pauses in the report pane.

### How it's measured

It reports behaviour, not mind-reading: every point is something you can replay.

- **Words:** Jev reads each thing the interviewer says, with one fast request per turn. It checks the tone, whether they reacted well, followed up on what you said, pushed back, sold the role, or mentioned next steps. Jev sees the text only: the previous question, the end of your answer, and their reply.
  - It needs your TypeSafe key, which you add in Setup.
  - Without the key, the timeline uses voices only, and says so.
- **Voices:** measured on this Mac and compared with the same person across the call. Your audio never leaves your Mac:
  - pitch, and how animated it is;
  - loudness;
  - pace;
  - how often the interviewer says "mm-hmm" while you talk.

### How well it works

- **Jev passed all six checks** on 102 labelled interviewer turns, at 91–100% balanced accuracy. The pass rule was written down before the run: `docs/eval/interviewer-decision.md`.
- **The scripted interviews:**
  - The strong one averages +0.59 and the weak one +0.05.
  - The strong one warms most after the 90-day-plan answer.
  - The weak one cools after the "my manager decides" answer.

### Known limits

- **Single-track recordings:** for imports from one mixed file, speaker detection decides who said what, so any mix-ups carry over into the timeline. The "mm-hmm" count needs separate tracks. The report says this on those interviews. Recordings made with the app have separate tracks.
- **Jev's habits:**
  - A vague "We'll be in touch" can still get a ★.
  - When the interviewer sells the role, the turn reads a little warmer.
  - A flat "I see." reads as neutral, not cool.
- **The greeting:** every interview opens warmly, so the greeting isn't counted for shifts or moments.
- **Short interviews:** your voice is compared only when there are at least 8 answers.

## Older interviews

Reports made before this version have no timeline. To add one without a new Claude analysis, run this in Terminal, with the interview's number in place of `<id>`:

```
"/Applications/Interview Coach.app/Contents/MacOS/ic" timeline <id>
```

Re-running the report in the app also adds one, but that's a new Claude analysis.

## Validation

- **Tests:** 113 Rust unit tests, 18 analysis and stage tests, the 3 end-to-end Whisper tests, and 29 Swift tests pass. Clippy is clean.
- **Live checks:**
  - the 102-turn Jev comparison;
  - the strong-vs-weak check on the scripted interviews, with real Jev and real audio (`tests/timeline_live.rs`);
  - a real single-track interview's timeline, rebuilt in 6 seconds on a copy of the data.
- **Report page:** looked at in light and dark mode.
- **Click to play:** checked in a WebKit test window. HTML and chart links both reached the app, and web links opened in the browser. It hasn't been clicked in the released app yet.
