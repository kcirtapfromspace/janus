# Janus

When an interview ends without useful feedback, Janus gives you another perspective. Record or
import the conversation, review the transcript and evidence behind your feedback, and prepare
for the next round.
Audio and video are processed locally on your Mac. Transcript text goes to your chosen coaching
provider (Claude or OpenAI); core Jev evaluation sends transcript excerpts to TypeSafe.

[Website](https://kcirtapfromspace.github.io/janus/) · [Download for Mac](https://github.com/kcirtapfromspace/janus/releases/latest)

Janus continues the existing Interview Coach update feed. Interviews, settings, and sign-ins
carry forward from earlier previews.

## Install on a Mac

Download the notarized app from **[Janus releases](https://github.com/kcirtapfromspace/janus/releases/latest)**,
move it to Applications, and open it. Its **Setup** window walks through browser sign-in with
Claude or ChatGPT, choosing a coaching model, downloading the local speech models, and a 5-second
recording test. Core coaching does not need Docker. ffmpeg and Anthropic's `ant` come inside the app. It updates
itself after that. Releasing is described in [docs/RELEASING.md](docs/RELEASING.md).

## Build from source

```sh
mac/build.sh               # builds `ic`, both Mac apps, and the bundled ffmpeg and ant into mac/build/ (signed)
open "mac/build/Janus.app"   # its Setup window does the rest
```

From the command line instead: `ic setup status` shows what's left, `ic setup run all` downloads
the speech models, and `ic login` signs you in to Claude. Use `ic login --provider openai` for ChatGPT.
(`cargo install --path .` puts `ic` on your PATH; outside the app it uses ffmpeg and `ant` from PATH,
or `IC_FFMPEG` / `IC_ANT`.)

### The Mac app

**Janus.app** lives in the menu bar (Roman profiles formed by opposing quotation marks):

- **Record interview** asks you to confirm everyone agreed to be recorded, then records your mic
  and the call's audio from any app, and (unless you turn it off) the call's window as video. The
  icon turns into a red record dot; **Stop** ends it, and the app transcribes and analyses the
  interview (about 1–2 minutes, plus a minute or two to read a long call's video).
- **Video** records only the call's window (Zoom, Teams, Webex, FaceTime, Slack, or a Meet, Teams
  or Zoom browser tab), never the rest of your screen. It needs Screen Recording permission
  (Setup asks; reopen Janus after allowing it); without it, interviews record audio only. The
  video plays with the audio on the Recording, Transcript and Review tabs, so a quote's timestamp
  shows how people looked when it was said. The review's **What the video showed** section
  (experimental) counts, for each of your answers, how many others were on camera, their nods,
  and how often they looked away. Faces are read on this Mac with Apple's Vision framework:
  positions and head angles only, never expressions, emotions or identities, and nothing is sent.
- The menu lists recent interviews with their verdicts; click one to open its review.
- **Open Janus** opens your interview notebook: the latest conversation, earlier
  interviews, and their reviews. Activity, conversation time, active roles, and recorded outcomes
  stay together on the page. Choose a time range or return to a review.
  Your searchable library stays on the left; each interview has **Recording**, **Transcript**,
  **Review**, and **Prepare** tabs. The toolbar keeps **Record/Stop** and **Import** close by; the
  review holds report versions, outcome tracking, and browser export.
- **Practice Interview** (menu bar, or **Practice** in the window) runs a mock interview.
  - **Questions:** the interviewer asks real questions from your earlier interviews. It starts
    with the company and role you name, then the questions you answered least well, and fills in
    with classic questions.
  - **Conversation:** it speaks out loud and listens to your answers, which are transcribed on
    this Mac. Each answer's transcript goes to your coaching model during the session, so it can
    choose its next question. It follows up the way an interviewer would (at most twice per
    question).
  - **Review:** the mock is recorded as a two-track interview and reviewed like a real one. It's
    tagged "practice" and kept out of your interview counts. The review's recording checks don't
  apply to it, so a mic that dropped out during practice isn't flagged.

  Echo cancellation keeps the interviewer's voice out of your track, but headphones are still
  clearer.
- **Appearance** offers **System**, **Light**, and **Dark** in the menu bar and Settings. System is
  the default; your choice is saved and applies to every window and embedded report.

The [brand assets and dashboard definitions](mac/Resources/Brand/README.md) describe the visual identity
and how the local analytics are counted.

The app records in-process (the first recording asks for Microphone and System Audio Recording
permission for "Janus") and hands everything else to the `ic` tool bundled inside it, so
the app and the command line share the same data in `~/InterviewCoach`.

### Models: Claude or OpenAI

Analysis works with either provider. The existing default remains `anthropic/claude-opus-5-5`.
Sign in and choose a model in Setup, or use the CLI:

```sh
ic login                                  # Claude browser sign-in
ic login --provider openai                # Continue with ChatGPT
ic proxy key typesafe                     # required Jev evaluation key
ic models                                 # account-specific available models
ic config set model openai/<model>         # choose a model from that list
ic analyze 3 --model openai/<model>         # one-off
```

**ChatGPT browser sign-in** uses OpenAI's documented OAuth/OIDC public-client flow: a local
`127.0.0.1` callback, PKCE, state and nonce verification, and signature-verified identity tokens.
Each installation keeps a stable host identifier, and each account/workspace registration keeps
its issued client ID. Setup offers saved accounts, **Add ChatGPT account**, **Sign out**, and
**Manage ChatGPT usage**. A new sign-in becomes active only after verification succeeds.

Eligible AI requests can use the user's ChatGPT plan when `chatgpt.tokens.use.direct` permission
is granted. Identity sign-in alone does not authorize inference. Choose **Enable plan usage**
if permission is missing. OpenAI currently documents this flow for open-source and locally hosted
apps; eligibility for a paid release remains a launch gate, not an assumption.
See [OpenAI's integration documentation](https://developers.openai.com/siwc/token-sharing-open-source).

Access tokens refresh near expiry, with a process lock to protect rotating refresh tokens.
Credentials are atomically saved with owner-only permissions (`0600`) under
`~/InterviewCoach/auth/openai.json`, in a `0700` directory. They are excluded from setup status,
logs, and support output. Signing out clears the selected account's tokens and attempts remote
revocation while retaining its registration and host ID for later sign-in. If remote revocation
cannot be confirmed, the app asks you to disconnect it in ChatGPT Settings.

**API billing is an explicit alternative.** `ic proxy key openai` retains its familiar command
name, but saves the key in private local storage without starting Docker. Adding a key selects
API billing; signing in with ChatGPT selects plan usage. An expired, denied, or usage-limited
ChatGPT session never silently switches to a stored API key. Existing OpenAI keys in the legacy
proxy's local `.env` remain usable until you select ChatGPT sign-in.

**Claude uses your browser login.** `ic login` runs Anthropic's bundled CLI, approves access in
the browser, and keeps the session in its dedicated `interview-coach` profile. Core requests go
directly to Anthropic's Messages API, with a fresh access token per attempt. Other tools' profiles
are unaffected. `ic logout` signs out of Claude; `ic logout --provider openai` signs out of ChatGPT.

Both providers use native structured-output adapters. Every stored analysis records its model,
and every response is validated against the Rust output type. OpenAI uses the public Responses
API with `store: false` and `stream: true`; success requires a completed response event. Disabling
response storage does not by itself establish a zero-retention policy. Native model catalogs do
not estimate prices or label a model cheapest; use the account's available models in Setup.

### Core Jev evaluation

Jev evaluates answer quality and interviewer-reaction signals in every completed analysis.
Configure your TypeSafe key in Setup, or run:

```sh
ic proxy key typesafe
```

The command stores the key privately in `auth/typesafe.json`. Jev calls TypeSafe's native
`/v1/systemone` API directly, so the normal workflow requires no Docker gateway.
If evaluation fails or returns incomplete checks, the analysis remains unfinished. Recordings,
transcripts and coaching are preserved; retrying reuses completed work and finishes the checks.
Jev is the default evaluator. Legacy `off` or Claude scorer settings no longer disable Jev in
the standard analysis workflow.

### Advanced proxies

Advanced users can set both `IC_LLM_URL` and `IC_LLM_KEY` to use a shared LiteLLM proxy for core
requests. Remove those overrides to use the native ChatGPT flow. This proxy mode uses its own
configured OpenAI API billing, rather than forwarding locally saved ChatGPT credentials.

Speech models download into `~/Library/Caches/InterviewCoach/models`. Set `HTTPS_PROXY` if model
downloads need a network proxy.

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
ic record --video                       # also record the call's window (Screen Recording permission)
ic import zoom_recording.mp4            # or import an existing recording (audio or video)
ic import --mic me.wav --system them.wav   # separate tracks, if you have them

ic list                                 # sessions, predicted verdict, actual outcome
ic transcript 3                         # full transcript with speakers and timestamps
ic report 3 [--full] [--open]           # analysis in the terminal, or as an HTML page
ic outcome 3 advanced                   # record what actually happened
ic swap 3                               # if a single-track import got You/Interviewer backwards
ic questions [--company Acme]           # every interviewer question from your reviews, merged, with your scores
```

Recording and importing both transcribe and analyse automatically. `ic transcribe N` and
`ic analyze N` re-run each step on its own. Earlier analyses are kept.

**Consent:** tell the people on the call that you're recording, and that it includes video when
it does; some places require every party's consent. `ic record` asks you to confirm this before
it starts.

## How it works

| Step | What happens |
|---|---|
| Capture | `ICRecorder.app` (Swift) records your mic and the system audio (Zoom, Teams, Meet, …) as two time-aligned tracks, so "you vs. interviewer" labels come free. With video on, ScreenCaptureKit records the call's window (720p, up to 10 fps, at most about 400 MB an hour) on the same clock. |
| Transcribe | ffmpeg → 16 kHz FLAC → whisper.cpp (large-v3-turbo, on the GPU) with voice-activity detection, so silence isn't transcribed. Single-track files go through speaker diarization. |
| Measure | Talk share, answer lengths, pace, filler words, questions asked, interruptions: computed in code, so they're comparable over time. |
| Analyse | The model (Claude or OpenAI) reads the transcript and metrics and returns a structured review: verdict + interviewer signals, 7-dimension rubric, question-by-question feedback, and the top 3 things to work on. Every quote is checked against the transcript. |
| Read the room | The interviewer's turns are timed and placed warm or cool from their words (Jev) and voice. With video, `ic-vision` (Apple Vision, on this Mac) finds faces a few times a second; `src/video.rs` follows them, tells your face apart by whose speech its mouth moves with, and counts the others' nods and looking away during each answer. Experimental: thresholds aren't yet validated on real calls. |
| Calibrate | `ic outcome` records the real result, so predicted verdicts can be compared with what actually happened. |

Data lives in `~/InterviewCoach/` (`coach.db` plus one folder per session with audio,
`video.mov` and `faces.json` when video was recorded, `transcript.md`, `analysis.json`, and
`report.html`).

## Tests

```sh
cargo test                                            # fast unit + integration tests (models are faked)
cargo test --release --test pipeline -- --ignored     # end-to-end on synthetic interviews made with `say`
```

The synthetic fixtures (`tests/common/mod.rs`, scripts in `tests/fixtures/*.txt`) prove the
pipeline is wired correctly. TTS voices are far easier to tell apart than real people, so they
don't prove speaker detection works on real calls.

## License

Janus is open source under the [MIT license](LICENSE). Bundled dependencies retain their own licenses.
