# Which interviewer checks count in the room's temperature

Written before the first run, so the result can't shape the rule. The code is `eval::decide` with `temperature::interviewer_set()`; its constants must match this page. The checks that pass are listed in `temperature::EVAL_PASSED`.

## What's compared

- **Arm:** Jev (`jev-latest`) only, for now. The Claude arms are skipped, as decided for the answer scorer.
- **Turns:** the 102 labelled interviewer turns in `tests/fixtures/interviewer_turns.jsonl`. Each has the question before it, the end of the candidate's answer, and what the interviewer says next.
  - **88 designed turns:** 23 small sets in which variants of one reply differ in a single cue, for example the same follow-up with and without praise, or a challenge instead of acceptance.
  - **14 turns from the two scripted interviews** (`tests/fixtures/strong.txt` and `weak.txt`).
- **Positives per check:**

  | Check | Positives |
  |---|---|
  | `positive_reaction` | 26 |
  | `builds_on_answer` | 54 |
  | `pushback` | 19 |
  | `selling` | 16 |
  | `next_steps` | 16 |

  Tone splits into 33 warm, 45 neutral and 24 cool.
- **Runs:** each turn is scored 3 times, with the tone options in a different order each run.
- **Checks:** the six in `temperature::interviewer_checks()`, with the wording Jev sees.

## The rule

A check passes when Jev meets **all** of these:

| Requirement | Applies to | Threshold |
|---|---|---|
| Balanced accuracy | `tone` | ≥ 0.80 |
| Balanced accuracy | Yes/no checks | ≥ 0.85 |
| Stability (same pick on every run) | All checks | ≥ 0.95 |
| Failed calls | All checks | None |

Not part of the rule:
- **Speed.** The timeline runs after the report, not while you wait.
- **Calibration.** It's reported, but the temperature compares each turn's probability with the interview's own typical turn, so a uniform bias cancels out.

**A check that fails** weighs nothing in the temperature. It also isn't marked on the chart, quoted in the moments, or named as a cue. It can be re-tested after its wording changes, with the same rule.

## Labelling conventions

The labels follow the option descriptions Jev sees. Where those leave room, these conventions apply:

- **Pushback and tone:** a sceptical challenge is `cool`. A gentle re-ask ("Right, but how would you prioritise if it were up to you?") or a friendly correction is `neutral`.
- **Praise:** generic praise of the conversation or of enthusiasm ("Great talking to you!") isn't `positive_reaction`, which is about what the candidate just said.
- **Following up:** `builds_on_answer` needs something specific from the answer. A new topic, or "Can you be more specific?", doesn't count.
- **Next steps:** a vague "We'll be in touch" isn't `next_steps`. Naming a round, a person, the recruiter following up, or a date is.
- **Selling:** neutral facts about the team ("six engineers and a designer") aren't `selling` on their own. Enthusiasm about the role or company, or non-public plans, is.

## Known limits

- **The turns are written, not transcribed.** Real interviewers are less tidy. The planned check on real turns, labelled from your own interviews, is added once there are a few two-track recordings. A check that does much worse there (more than 0.10 below its score here) stops counting.
- **Small categories.** Each yes/no check has 16–54 positives, so a few disagreements move its score by several points. Treat results near a threshold as uncertain.
- **One labeller.** The labels are one person's reading. Turns where Jev and the label disagree are listed in the run's output for a second look. A label is only changed for a clear mistake, and any change is noted here.

## Result: first run, 2 October 2026

Jev (`typesafe/jev-1.13.0`), 102 turns × 3 runs: 306 calls, none failed, p50 115 ms, p95 200 ms. **All six checks pass**, so all six are in `EVAL_PASSED`.

| Check | Balanced accuracy | Floor | Stability | ECE (not gated) |
|---|---|---|---|---|
| `tone` | 0.91 | 0.80 | 0.97 | 0.05 |
| `positive_reaction` | 0.94 | 0.85 | 0.99 | 0.04 |
| `builds_on_answer` | 0.92 | 0.85 | 1.00 | 0.04 |
| `pushback` | 0.95 | 0.85 | 0.99 | 0.09 |
| `selling` | 1.00 | 0.85 | 1.00 | 0.06 |
| `next_steps` | 0.98 | 0.85 | 1.00 | 0.05 |

No labels were changed. The disagreements (listed in `dist/eval/interviewer/summary.md`) fall into a few patterns worth knowing when reading a timeline:

- **Selling reads as praise.** When the interviewer talks up the role, Jev usually also says `positive_reaction`, which nudges those turns warmer.
- **A flat "I see." or "Okay." before the next question reads as neutral**, not cool. A weak interview's coolness is under-read: it shows as a run of neutral turns, not cool ones.
- **A vague close counts as next steps.** "We'll be in touch" and "What's your timeline for making a move?" get `next_steps`, so a brush-off can carry a ★.
- **Re-asks count as following up.** "Can you be more specific?" gets `builds_on_answer`.
- **Gentle re-asks and corrections read as cool** in tone, where the labels say neutral.

## Change to the formula after the first live run

The plan counted each yes/no cue relative to the interview's median turn. On the strong scripted interview, that made plain questions read cool (around −0.35), with no cue to show for it, because most of its turns had a positive reaction. So `timeline-v1` uses each cue's probability directly. Tone stays P(warm) − P(cool), and voice stays relative to the speaker's own call.

Two more rules came out of the same run:
- **Colour and moments need a clear signal.** A turn shows in colour only at ±0.3 or beyond. It's quoted as a moment only then, and only with at least one named cue.
- **The opening greeting is skipped** for shifts and moments. It comes before your first answer, so it can't be a reaction to you, and every interview opens warmly.

On the scripted interviews, the strong one averages +0.59 and the weak one +0.05 (`tests/timeline_live.rs`):
- **Strong:** the room warmed most after the 90-day-plan answer.
- **Weak:** it cooled most after the "my manager decides" answer.
