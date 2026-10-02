# Interview Coach 0.1.0 preview 1 — Record an interview, get an honest read on it

The first preview. Interview Coach records your job interviews from any app, transcribes them on your Mac, and tells you how each one went and what to work on next.

## Install

Needs an Apple silicon Mac with macOS 14.4 or later.

1. Install the tools it relies on:
   - **Docker Desktop**, from docker.com, then open it once.
   - In Terminal: `brew install ffmpeg anthropics/tap/ant`
2. Download **InterviewCoach-v0.1.0-preview.1-macos-arm64.zip** below and unzip it.
3. **Move Interview Coach.app to Applications before opening it.** Opened from Downloads, macOS runs it from a temporary read-only copy, which can't update itself.
4. Open it. A waveform icon appears in the menu bar. Click it, then **Sign in to Claude…**. Terminal opens, starts the local LLM proxy, and opens your browser so you can approve access.
5. Optional, for the `ic` command line:

   ```sh
   ln -s "/Applications/Interview Coach.app/Contents/MacOS/ic" /opt/homebrew/bin/ic
   ```

The first transcription downloads the speech model (about 1.6 GB) once. After that, the app updates itself, and only installs updates when nothing is being recorded or analysed.

## What it does

- **Record from the menu bar.** It asks you to confirm everyone agreed to be recorded, then records your mic and the call's audio from any app (Zoom, Meet, Teams, and others) as separate tracks. Those separate tracks are how it tells you apart from the interviewer.
- **Transcribe on the Mac** with whisper.cpp on the GPU. Uploaded single-track recordings get speaker detection.
- **Measure** talk share, answer length, pace, filler words, and the questions you asked.
- **Analyse** with Claude:
  - a verdict, based on the interviewer's signals;
  - a 7-part rubric;
  - question-by-question feedback;
  - the 3 most useful things to work on.
  
  Every quote is checked against the transcript.
- **Record the real outcome** later, so predictions can be compared with what actually happened.

## Privacy

Audio never leaves the Mac. Only the transcript text goes to Claude, through a LiteLLM proxy running in Docker on your Mac. Claude uses your browser login, so no API key is stored anywhere.

## Validation

- **Tests:** 45 Rust tests, 3 end-to-end tests that run Whisper and speaker detection on scripted interviews, and 10 Swift tests all pass. Clippy reports no warnings.
- **Live analysis** through a browser login and the proxy:
  - the scripted weak interview was judged **Weak** and the strong one **Strong**, both at high confidence;
  - every quote was found word for word in the transcript;
  - each analysis cost about $0.09.
- **Apple:** the release is notarized, the ticket is stapled, and Gatekeeper accepts it.

## Known limits

- **Recording:** the recorder hasn't yet been tested on a real call. The first recording asks for Microphone and System Audio Recording permission. If the interviewer's side comes out silent, check System Audio Recording.
- **Question count:** "Questions you asked" also counts rhetorical ones ("…you know?").
- **OpenAI:** analysis with OpenAI needs an API key (`ic proxy key openai`), because OpenAI has no browser login.
