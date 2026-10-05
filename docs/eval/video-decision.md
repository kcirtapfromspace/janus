# Which video cues count in a review

**Draft for review.** The rule below freezes when the first designed clips are labelled, before
anything is scored, so the results can't shape it. This is Jev's discipline applied to the
call's video: a cue is shown only after it passes a test against labelled clips. The code will
be `eval::decide` with `video::video_set()`; its constants must match this page. Checks that pass
go in `video::VIDEO_PASSED`.

Until then, preview.14's **What the video showed** stays marked experimental.

## What's scored

**A clip:** 20 seconds of the call's video while you're answering. An answer of 10–20 seconds is
one clip. A longer answer gives up to three clips that don't overlap: its middle (20–40 s), its
start and end (40–60 s), or all three (60 s or more). Answers shorter than 10 seconds give none.
Clips are short so a person can label them reliably, and so counts mean the same thing from clip
to clip. They sample an answer rather than cover it, so the rule tests the measurements, not the
review's per-answer totals.

**The checks**, all about the people on camera other than you:

| Check | Kind | Asks | Floor |
|---|---|---|---|
| `nodded` | yes/no | Did anyone else nod at least once? | balanced accuracy ≥ 0.85 |
| `nod_count` | levels: none, 1–2, 3–5, 6+ | How many nods, across everyone else? | within one level ≥ 0.90 |
| `looked_away` | choice: rarely (under 10%), sometimes (10–40%), mostly (over 40%) | How much of their time on camera were they turned away from the screen? | balanced accuracy ≥ 0.80 |
| `on_camera` | choice: 0, 1, 2, 3+ | How many others were on camera for most of the clip? | balanced accuracy ≥ 0.90 |
| `smiled` | yes/no | Did anyone else visibly smile? | balanced accuracy ≥ 0.85 |

`smiled` is labelled from the start but scored only by an arm that measures smiles. `video-v1`
doesn't, so the check can be tested later without relabelling.

**Who is you**, as a separate set (`video_you`). One item is one interview, labelled with a
point on your face at one moment, or with your face not being in the video (camera off). The
track at that point is yours; every other track present then is someone else's. Floor: across
those tracks, accuracy ≥ 0.95, and your track counted as someone else in at most 5% of
interviews. That second limit is the one that matters: when it happens, your own head movements
while talking become "their nods". In an interview where your face isn't in the video, any track
called yours is wrong.

## Arms

| Arm | What it is | Runs on |
|---|---|---|
| `video-v1` | `src/video.rs` as shipped in preview.14, with its thresholds frozen (`NOD_DROP` 0.06, `NOD_WINDOW_S` 0.8, `AWAY_YAW` 30°, `AWAY_PITCH` 25°, `MIN_FACE_H` 0.04) | this Mac |
| `video-v2` (candidate) | MediaPipe face landmarks and blendshapes: adds smiles and steadier head pitch. Its thresholds are tuned on the designed clips only | this Mac |
| `opus-frames` (optional baseline) | Claude Opus 5.5 looking at 1-frame-per-second stills. Claude takes images, not video, so it can't score `nodded` or `nod_count`. Designed clips only | Anthropic API |

Per check, the cheapest qualifying arm wins (`video-v1` first, then `video-v2`, then
`opus-frames`), as in `eval::decide`. A cloud arm is a baseline for comparison. It is never used
on real interviews unless the user opts in, because the review promises that nothing about
faces leaves the Mac.

**Runs:** each clip is scored 3 times, each time sampling the frames at a different starting
point. `ic-vision` reads the clip once at 10 frames per second, and each run takes every other
sample starting from a different frame. This is the video version of rotating Jev's options.
A cue that flips when its frames shift by a sixth of a second sits on a threshold edge.

## The rule

A check passes when its arm meets **all** of these on the **realistic set** (below):

| Requirement | Threshold |
|---|---|
| Its floor in the table above | as listed |
| Stability (same pick in all 3 runs) | ≥ 0.95 |
| No more than this below its designed-set score | 0.10 |
| Failed clips (the tool errored, or no video) | none |
| Positives in the realistic set, per yes/no answer and per choice option | at least 30 before the run counts |

**Tile size:** results are also broken down by the size of the face in the frame: under 8%,
8–15%, and over 15% of the frame's height. If a passing check falls below 0.75 within one size
band, the review hides that cue for faces of that size rather than dropping the check. Face size
is something the product can measure; layout isn't, so layout is reported but not used as a
gate.

**A check that fails** is not shown: it's not counted, named or summarised. It can be re-tested
after the method changes (`video-v2`, new thresholds) with the same rule, on clips the change
was not tuned on.

## Clips and labels

**Designed set** (tuning): staged calls among 3–5 volunteers on real call apps. Each short scene
varies one thing at a time:
- a nod or no nod, single or repeated
- looking at the screen or down at notes
- glasses or none
- gallery or speaker view
- 2, 3 or 5 tiles
- normal lighting or a window behind

Aim for about 120 clips. Thresholds may be tuned here only.

**Realistic set** (the test): mock interviews over Zoom, Meet and Teams with volunteers playing
interviewers, plus the user's own real interviews, but only where everyone agreed to this
further use. Being recorded for an interview isn't consent to have your face used to test
software. Aim for about 150 clips from at least 10 calls and at least 6 different interviewers.
Never used for tuning.

**Storage:** the repository is public. No video is ever committed. Designed-clip labels and
their `faces.json` excerpts (face positions and angles, never images) may be committed when every
volunteer agrees. The realistic set's labels and faces stay in `~/InterviewCoach/eval/video/`, and
only its aggregate results appear on this page.

**A label line** (JSONL, the same `eval::Item` shape as the other sets):

```json
{"id": "s003-a07-c2", "set": "s003", "variant": "a07-c2", "origin": "realistic",
 "session_dir": "~/InterviewCoach/sessions/0003-acme-hm", "start": 412.0, "end": 432.0,
 "layout": "gallery", "face_height": 0.11,
 "labels": {"nodded": true, "nod_count": "1-2", "looked_away": "sometimes", "on_camera": "2", "smiled": false}}
```

and for `video_you`, one line per interview (`you.jsonl`):
`{"id": "s003-you", "set": "s003", "variant": "you", "session_dir": "…", "at": 95.0, "x": 0.78, "y": 0.62, "labels": {"you_visible": true}}`.
The point (x, y), as fractions of the frame from its top-left, at second `at` picks out your track.
With `"you_visible": false`, `at`, `x` and `y` are null.

## Labelling conventions

- **Label before looking:** label a clip in `ic eval label` (below), at half speed if needed,
  before opening its review or `faces.json`. The labelling page shows nothing Janus measured.
- **Nod:** a deliberate down-and-up of the head, counted once per dip. Repeated nods count each
  dip. A head that moves along with speech, a laugh, or shifting in the seat isn't a nod.
- **Looking away:** the face clearly turned off the screen (aside, down at a desk or a phone) for
  at least a second. Glancing at notes counts. This measures where the face points, not whether
  the person is engaged, and the review says so.
- **On camera:** a person whose face is visible for most of the clip. A camera that's off, or
  only a name or avatar, isn't on camera.
- **Smile:** the corners of the lips visibly raised, with or without teeth. A polite half-smile
  counts; a neutral resting mouth doesn't.
- **Two labellers:** a second person labels a quarter of the realistic clips. Agreement
  (Cohen's kappa) is reported per check. A check where people agree below 0.70 is redefined
  before it is scored: a detector can't be tested against labels that people can't agree on.
  The second person labels their own copy: `ic eval clips <ids> --dir ~/InterviewCoach/eval/video-second`,
  then `ic eval label --dir ~/InterviewCoach/eval/video-second`. Clip ids come out the same, so
  the two files join by `id`. The agreement report is still code to add.

## Labelling

Built (`src/label.rs`, `src/label.html`):

```sh
ic eval clips 12 14 --origin realistic   # cut interviews recorded with video into clips
ic eval label                            # label them in the browser; Ctrl+C when done
```

- **`ic eval clips`** writes clips to `~/InterviewCoach/eval/video/clips.jsonl`; `--dir` puts
  them elsewhere. Every interview given is checked before anything is written. Adding an
  interview again adds only its new clips and keeps every label. If the transcript was re-run and
  a listed clip's window moved, it's reported and left as labelled. Each clip records the median
  height of all the faces in it, yours included (the size band), when `faces.json` exists.
- Changes hold a lock on the folder, so adding interviews while the labelling page is open
  can't undo a label, and saving a label can't drop a newly added clip.
- **`ic eval label`** serves a page to this Mac only. Every request needs the random token in
  the printed link and a 127.0.0.1 or localhost Host header. The video is served by its interview,
  never by a path from the page. The page:
  - loops each clip, and has half speed;
  - asks every question, with "Can't tell" for each check;
  - keeps the answers consistent (no nods means "none");
  - asks once per interview which face is yours, by clicking it.

  The server checks the labels against these conventions again before saving, and each save
  rewrites the file whole, through a temporary file.
- **`you.jsonl`** has one line per interview: the moment and point you clicked, or that your face
  isn't in the video.
- The page plays the recorder's video in Chrome, which was tested with the labelling page.
  Safari is expected to work, since it plays QuickTime natively, but hasn't been tried.

## Code to add when this is adopted

- `video::video_set()` and `video::you_set()` as `CheckSet`s. Their `input_from` builds a
  `ClipInput` (`session_dir`, `start`, `end`) whose state is that window of `faces.json`.
- `ArmKind::Local { method }` in `eval.rs`, and a `VideoScorer: Scorer` that runs `video.rs` on
  the clip. Its `rotation` is the sampling offset. Each yes/no check returns probability 0 or 1
  (`video-v1` isn't probabilistic, so calibration isn't gated).
- `ic eval scorers --set video --items <path>`, defaulting to `tests/fixtures/video_clips.jsonl`
  for the designed set.
- `video::VIDEO_PASSED` and the tile-size bands, gating `video::notes` and the review section the
  way `temperature::EVAL_PASSED` gates the timeline. The "experimental" tag comes off only for
  checks that pass.

## Known limits

- **Small realistic set.** At 150 clips, a few disagreements move a score by several points.
  Treat results near a floor as uncertain.
- **Volunteers act.** Mock interviewers nod more deliberately than real ones. The realistic set
  limits this but doesn't remove it.
- **One kind of camera feed.** Every clip comes through a call app's compression and tiling at
  1280×720. Other resolutions aren't tested.
- **Not a reading of feelings.** Passing means the cue matches what a person sees in the video.
  It doesn't mean a nod is agreement or looking away is disinterest, and the review never
  claims either.
