# Interview Coach

Record your job interviews, get a full transcript, find out how each one really went, and get
coached on what to improve. It runs locally on your Mac: audio never leaves the machine, and only
the transcript text goes to the model you choose (Claude or OpenAI) for analysis.

## Install on a Mac

Download the notarized app from **[interview-coach-releases](https://github.com/kcirtapfromspace/interview-coach-releases/releases/latest)**,
move it to Applications, and open it. Its **Setup** window walks through the rest: Docker Desktop (the
only thing to install yourself), the local AI proxy, signing in to Claude in your browser, the speech
models, and a 5-second recording test. ffmpeg and Anthropic's `ant` come inside the app. It updates
itself after that. Releasing is described in [docs/RELEASING.md](docs/RELEASING.md).

## Build from source

```sh
mac/build.sh               # builds `ic`, both Mac apps, and the bundled ffmpeg and ant into mac/build/ (signed)
open "mac/build/Interview Coach.app"   # its Setup window does the rest
```

From the command line instead: `ic setup status` shows what's left, `ic setup run all` starts Docker,
sets up the AI proxy and downloads the models, and `ic login` signs you in to Claude.
(`cargo install --path .` puts `ic` on your PATH; outside the app it uses ffmpeg and `ant` from PATH,
or `IC_FFMPEG` / `IC_ANT`.)

### The Mac app

**Interview Coach.app** lives in the menu bar (waveform icon):

- **Record interview** asks you to confirm everyone agreed to be recorded, then records your mic
  and the call's audio from any app. The icon turns into a red record dot; **Stop** ends it, and
  the app transcribes and analyses the interview (about 1–2 minutes).
- The panel lists your 5 most recent interviews with their verdicts; click one to open it.
- **Open Interview Coach** shows the window: your interviews on the left, the selected report on
  the right, and a toolbar with **Record/Stop**, **Import**, **Analyze**, **Outcome**, **Open in
  Browser**, and **Show in Finder**.

The app records in-process (the first recording asks for Microphone and System Audio Recording
permission for "Interview Coach") and hands everything else to the `ic` tool bundled inside it, so
the app and the command line share the same data in `~/InterviewCoach`.

### Models: Claude or OpenAI

Analysis works with either provider. The default is `anthropic/claude-opus-5-5`; switch per command
or permanently:

```sh
ic analyze 3 --model openai/gpt-5.6        # one-off
ic config set model openai/gpt-5.6         # new default (saved to ~/InterviewCoach/config.toml)
ic proxy key openai                        # OpenAI needs an API key in the proxy (see below)
```

**Claude uses your browser login — no API key.** `ic login` runs Anthropic's CLI
(`brew tap anthropics/tap && brew install anthropics/tap/ant`), which opens the browser so you can
approve access and then keeps the session (a short-lived access token plus a refresh token) in its
own credential store. Before each Claude request, `ic` asks it for a fresh access token; LiteLLM
forwards that token to Anthropic. The session is saved as an `interview-coach` profile used only by
`ic`, so it doesn't change the account other tools (such as Claude Code) use. If the login ever
expires, `ic doctor` says so; run `ic login` again.

**OpenAI's API has no browser login**, so it's opt-in: `ic proxy key openai` opens OpenAI's key page
and stores the key in the proxy (never in `ic`).

Each provider has a native adapter (Claude's Messages API, OpenAI's Responses API), both behind one
typed interface: the JSON schema the model must follow is generated from the Rust `SessionAnalysis`
type, and every answer is parsed and validated back into it, whichever model wrote it. Each stored
analysis records the `provider/model` that produced it. OpenAI requests are sent with `store: false`,
so transcripts aren't kept on OpenAI's servers.

### The LLM proxy

Every LLM request goes through a local [LiteLLM](https://docs.litellm.ai) proxy running in Docker
on `127.0.0.1:4000` (loopback only), using its pass-through routes (`/anthropic/v1/messages`,
`/openai_passthrough/v1/responses`) so requests reach each provider unchanged. `ic proxy setup`
(run for you by `ic login`) writes its config to `~/InterviewCoach/litellm/` (secrets in `.env`,
readable only by you), starts LiteLLM + Postgres, and gives `ic` its own LiteLLM virtual key: a
local credential between `ic` and the proxy, generated automatically, that you never handle.
Re-running setup is safe: it only fills in what's missing.

```sh
ic proxy status            # is it up, and what has ic spent through it
ic proxy stop / start      # containers stop; keys and spend history are kept
```

To use a different LiteLLM proxy, set `IC_LLM_URL` and `IC_LLM_KEY`. The Whisper model (~1.6 GB) and
the speaker-detection models download directly (not via LiteLLM) the first time they're needed,
into `~/Library/Caches/InterviewCoach/models`. Set `HTTPS_PROXY` if those downloads need a proxy too.

### Settings

`ic config show` prints the settings in effect; `ic config set <key> <value>` changes them, rejecting
invalid values before saving. The file (`~/InterviewCoach/config.toml`) is parsed strictly: unknown
keys and malformed values are errors that name the offending key.

| Key | Environment override | Default |
|---|---|---|
| `model` | `IC_MODEL` | `anthropic/claude-opus-5-5` |
| `language` | `IC_LANGUAGE` | `en` (`auto` to detect) |
| `whisper_model` | `IC_WHISPER_MODEL` | `large-v3-turbo` |
| `models_dir` | `IC_MODELS_DIR` | `~/Library/Caches/InterviewCoach/models` |

`IC_DATA_DIR` moves the whole data folder (default `~/InterviewCoach`).

## Use

```sh
ic record --company Acme                # or record from the terminal; Ctrl+C to stop
ic import zoom_recording.mp4            # or import an existing recording (audio or video)
ic import --mic me.wav --system them.wav   # separate tracks, if you have them

ic list                                 # sessions, predicted verdict, actual outcome
ic transcript 3                         # full transcript with speakers and timestamps
ic report 3 [--full] [--open]           # analysis in the terminal, or as an HTML page
ic outcome 3 advanced                   # record what actually happened
ic swap 3                               # if a single-track import got You/Interviewer backwards
```

Recording and importing both transcribe and analyse automatically. `ic transcribe N` and
`ic analyze N` re-run each step on its own. Earlier analyses are kept.

**Consent:** tell the people on the call that you're recording; some places require every party's
consent. `ic record` asks you to confirm this before it starts.

## How it works

| Step | What happens |
|---|---|
| Capture | `ICRecorder.app` (Swift) records your mic and the system audio (Zoom, Teams, Meet, …) as two time-aligned tracks, so "you vs. interviewer" labels come free. |
| Transcribe | ffmpeg → 16 kHz FLAC → whisper.cpp (large-v3-turbo, on the GPU) with voice-activity detection, so silence isn't transcribed. Single-track files go through speaker diarization. |
| Measure | Talk share, answer lengths, pace, filler words, questions asked, interruptions: computed in code, so they're comparable over time. |
| Analyse | The model (Claude or OpenAI) reads the transcript and metrics and returns a structured review: verdict + interviewer signals, 7-dimension rubric, question-by-question feedback, and the top 3 things to work on. Every quote is checked against the transcript. |
| Calibrate | `ic outcome` records the real result, so predicted verdicts can be compared with what actually happened. |

Data lives in `~/InterviewCoach/` (`coach.db` plus one folder per session with audio,
`transcript.md`, `analysis.json`, and `report.html`).

## Tests

```sh
cargo test                                            # fast unit + integration tests (models are faked)
cargo test --release --test pipeline -- --ignored     # end-to-end on synthetic interviews made with `say`
```

The synthetic fixtures (`tests/common/mod.rs`, scripts in `tests/fixtures/*.txt`) prove the
pipeline is wired correctly. TTS voices are far easier to tell apart than real people, so they
don't prove speaker detection works on real calls.
