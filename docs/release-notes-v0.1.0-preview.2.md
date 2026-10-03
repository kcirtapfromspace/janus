# Interview Coach 0.1.0 preview 2 — Review each interview in stages, and a recorder fix

**Update before your next interview.** In preview 1, the recorder could stop capturing your microphone partway through a call while the interviewer's side kept recording, so your answers were lost. Preview 2 fixes that and warns you while recording if it ever happens again.

## Recorder fix

- **What went wrong:** switching to headphones during a recording, or a call app (Zoom, Meet, Teams) reconfiguring the microphone, changes the input device. Preview 1 restarted its audio engine on the old device, which reported success but captured nothing more.
- **Reconnecting:** the recorder now builds a fresh audio engine on whichever mic is the default. It converts the audio if the mic's sample rate changes, which Bluetooth headsets do on calls, and gives a new headset a few seconds to start before retrying. A watchdog rebuilds the engine whenever no mic audio has arrived for 3 seconds.
- **Devices used:** the recording now lists every mic it used, e.g. "MacBook Pro Microphone → AirPods Pro".
- **Live warning:** if either track stops receiving audio while you're recording, the menu-bar icon turns into a warning triangle and the panel says what's wrong.
- **Recorder report:** it now flags a track that stopped early or dropped out, and says when.
- **Reports on affected recordings:**
  - they open with a notice saying what's missing;
  - Claude is told the gap is a recording fault, not silence, so it judges from what was captured and doesn't score answers it can't hear;
  - talk-time numbers are left out rather than shown wrong.
- **Timestamps:** a long stretch of one side is now split where the other side's speech is missing, so quotes keep their real timestamps instead of all pointing at the same moment.

**Already recorded an interview with preview 1?** Open it, select **After-action report**, and choose **Re-run**. The new report uses the notes above. Answers the recorder didn't capture can't be recovered.

## Review in stages

Each interview is now shown as four stages: **Recording → Transcript → After-action report → What to do next**.

- **The flow strip** shows each stage's status, a one-line summary, and when it last ran.
- **Re-run any stage on its own.** For example, re-transcribe, or re-run the report with a different model. Later stages are then marked out of date, and **Update later steps** brings them current.
- **What to do next** is new:
  - **next-round prep** comes from what the interviewer actually said, with quotes and likely questions;
  - a **practice plan** turns the report's coaching into timed drills you can tick off.
- **Recording:** listen to the interview in the app.
- **Transcript:** click a line to play it from that moment.
- **History:** every report run is kept, and you can switch between them.

## Validation

- **Tests:** 64 Rust tests, 3 end-to-end tests that run Whisper and speaker detection, and 20 Swift tests all pass, and clippy reports no warnings. New tests cover:
  - a track that stops early or drops out;
  - the live recording warning;
  - the report notice and leaving metrics out;
  - out-of-date stages and re-runs;
  - upgrading an existing database.
- **The interview that exposed the bug** was re-processed from its original audio:
  - the report opens with "Your microphone stopped recording at 0:25 of 42:11";
  - the summary describes the interview itself;
  - 13 interviewer questions are reviewed, up from 6, each with its real timestamp instead of all at 00:00:30;
  - every quote was found word for word in the transcript.
- **Apple:** the release is notarized with no issues, the ticket is stapled, and Gatekeeper accepts a downloaded copy.
- **Not yet tested:** switching headphones during a real recording with this build.

## Known limits

- The recorder fix can't be fully tested without a real call. Before relying on it, make a short test call and switch headphones on and off during it. Then check that the Recording stage shows no warnings.
- "Questions you asked" also counts rhetorical ones ("…you know?").
- Analysis with OpenAI needs an API key (`ic proxy key openai`).
