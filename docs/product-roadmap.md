# Product launch priorities

Working positioning: **Turn every interview into a better next interview.**
Initial customer hypothesis: Mac-using professionals actively interviewing. Validate it in the
private beta before expanding platforms or building a broad application tracker.

| Order | Deliverable | Acceptance criteria | Status |
| --- | --- | --- | --- |
| 1 | ChatGPT sign-in and simple setup | Native provider requests; browser sign-in; verified identity; safe refresh/logout; explicit billing; account-specific model picker; first report without Docker | Implemented in source; live sign-in and provider inference validation pending |
| 2 | Recording reliability | Test permissions, Bluetooth/device changes, long calls, sleep, crashes and interrupted processing on supported Macs; no silent loss of the user's recording | Startup cancellation and exclusive session ownership tested; real-device matrix pending |
| 3 | Trustworthy coaching | Consented real-interview dataset; independent human labels; held-out validation across formats and speakers; quote grounding; uncertainty shown for inferred signals | Scripted evaluations exist; real-interview validation pending |
| 4 | Practice and preparation | Select one improvement, record a practice answer, compare it with the original, and prepare for the next round using role context | Written plans exist; interactive loop pending |
| 5 | Brand and commercial launch | Cleared name/domain; consistent identity; tested offer/pricing; billing/licensing; website/demo; support; privacy and terms | Decisions and implementation pending |

## Current implementation batch

- Core Claude and OpenAI reports use native APIs. A new install does not require Docker, LiteLLM,
  or Postgres to get a coaching report.
- ChatGPT uses the documented dynamic public-client OAuth flow. Identity and plan permission are
  checked separately; the app retains separate registrations and supports reauthorization.
- Credentials are privately and atomically stored, token refresh is locked across processes, and
  sign-out attempts remote revocation. No tokens enter setup status or model menus.
- OAuth failure never silently switches to paid API billing. Adding a key explicitly chooses
  API billing; signing in explicitly chooses ChatGPT plan usage.
- Jev is a core evaluator, using TypeSafe directly without Docker. Completed analyses require
  complete answer and interviewer checks. Failures preserve artifacts and support cached retries.
  Setup requires a TypeSafe key and explains the excerpts sent for evaluation.
- The native model picker uses account-specific provider catalogs. It does not invent prices or
  assume a ChatGPT account can access a hard-coded model.

## Gates before releasing this batch

- Complete a real browser sign-in, model listing, structured coaching request, refresh,
  account switch, denied-permission recovery, usage-limit recovery, and sign-out.
- Confirm paid-product eligibility with OpenAI before promising ChatGPT plan usage commercially.
  The repository is private; a local technical integration does not establish commercial approval.
- Verify direct Claude authentication with the bundled CLI on a clean Mac.
- Verify Jev end to end with a real TypeSafe account, including failed and retried evaluations.
- Decide whether launch uses customer TypeSafe keys or a managed backend with usage limits.
- Run the clean-install flow without Docker and verify existing local keys and data still work.
- Inspect Setup and account/model menus in the signed app; preserve permissions across updates.
- Run release validation and notarization before publishing a new archive or update feed.

## Next implementation batches

**Recording reliability:** make preflight audio checks visible at the point of recording;
provide actionable recovery after interruption; verify retained audio and stage resumability.
Exercise actual supported devices and call apps, alongside the existing synthetic tests.

**Coaching quality:** keep observations, actionable coaching and predicted hiring outcomes
visually distinct. Evaluate interviewer sentiment separately from outcome prediction. Gather
user corrections and actual outcomes without collecting recordings or transcripts by default.

**Practice loop:** attach exercises to specific answers and evidence, record another attempt,
and compare the same rubric across attempts. Add explicit résumé, job description, target level
and goals as optional context with clear provider disclosures.

**Brand and business:** NextRound is a naming candidate, not an approved name. Check competitors,
domains and trademarks, then update the visible identity while preserving existing data paths,
signing identity and update compatibility. Test a job-search pass against a subscription; decide
how API usage, Jev usage, limits, refunds and licensing work before implementing checkout.

## Beta evidence

Track install-to-first-report completion, recording success, actionable feedback ratings,
completed practice exercises and return usage during an active job search. Use opt-in,
content-free diagnostics; redact tokens, transcript text and identifiable interview details.
Set release thresholds before reviewing results, and document failures instead of treating
scripted evaluation scores as proof of real-world accuracy.

Defer cloud sync, Windows and calendar integrations until the core improvement loop earns
repeat use. A local product does not need a hosted backend merely to be launchable. The one
exception, decided 2026-10-05, is the opt-in shared question registry: a free Cloudflare Worker
and D1 database (cloud/registry), until volume calls for something sturdier. Interviews
themselves stay local.

## Recording reliability: first implementation

- Stopping during the microphone permission prompt permanently closes that recording attempt;
  a late permission response cannot start capture.
- An OS file lock protects each recording folder before PID and audio checks. Duplicate starts
  cannot overwrite existing audio, and stopping releases ownership.
- The app reserves Record before asynchronous preparation, preventing duplicate sessions from
  rapid clicks across windows.
- Recorder errors return failure status; failure to save the final report leaves captured files
  in place and surfaces an error instead of automatically starting analysis.
- Six regression tests cover cancellation, repeated start, ownership/release, preserving audio,
  report-save failure, and denied permission.
- Still required: live microphone/system-audio checks, Bluetooth changes, sleep/resume, disk-full
  capture, and long-call soak testing on supported Macs.
