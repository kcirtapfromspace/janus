# How Interview Coach picks its answer scorer

Written before the first comparison run, so the result can't shape the rule. The code is `eval::decide` in `src/eval.rs`; the constants there must match this page.

## What's compared

- **Arms:**

  | Arm | Model | Settings |
  |---|---|---|
  | Jev | `jev-latest` | |
  | Haiku | `claude-haiku-4-5-20251001` | no thinking |
  | Sonnet | `claude-sonnet-5-5` | effort low |
  | Opus | `claude-opus-5-5` | effort low |

- **Answers:** the 70 labelled answers in `tests/fixtures/answers.jsonl`:
  - 20 strong answers, each with two variants that change one thing (the point buried, no number, "we" instead of "I", a missing STAR part, rambling, or a result in words such as "doubled");
  - 10 answers from the scripted interviews.
- **Runs:** each arm scores every answer 3 times. Choice options are shown in a different order each run.
- **Checks:** the six built-in checks in `src/scoring.rs`, with identical wording for every arm.

## The rule

For each check, the winner is the cheapest, fastest arm that meets **all** of these. Arms are ordered from cheapest to most expensive: Jev, Haiku, Sonnet, Opus.

| Requirement | Applies to | Threshold |
|---|---|---|
| Balanced accuracy | Yes/no and choice checks | ≥ 0.85 |
| Distance behind the best arm's balanced accuracy | Yes/no and choice checks | ≤ 0.05 |
| Within-1 agreement | 1–5 level checks | ≥ 0.90 |
| Stability (same pick on every run) | All checks | ≥ 0.95 |
| p95 latency per answer | All checks | ≤ 1 s, so drills feel instant |
| Expected calibration error of its pick probability | Jev only | ≤ 0.10, so its confidence can be trusted for routing |
| Failed calls | All checks | None |

**If no arm qualifies for a check**, the report shows each arm's reasons and names the most accurate arm, which is used as the fallback.

**A cascade is acceptable** if it beats every single arm: use Jev's answer when its pick probability is at or above a threshold, and ask the best Claude arm otherwise. To qualify, its accuracy must be within 0.02 of the best arm, while escalating at most 25% of answers.

## Known limits

- **Small categories:** these have 4–12 examples each ("mixed" ownership, each missing STAR part, buried points). Treat their numbers as rough.
- **Synthetic answers:** the designed answers are written, not transcribed speech. A check that does well here but badly on real answers will be caught when real answers are added.
- **The real-answer test:** the planned check on 20–30 of your own labelled answers needs real interviews with both tracks, and there aren't any yet. It's added the first time they exist. If a check's accuracy on them is more than 0.10 below its accuracy here, it doesn't count as passing.
