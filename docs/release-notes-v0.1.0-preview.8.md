# Interview Coach 0.1.0 preview 8 — Same inputs, same report, and every version kept

## Re-running gives the same report back

A report is now stored under its exact inputs: the transcript, the call stats and recording notes, the model, and the prompt. Re-running with nothing changed shows that same version again.
- **Nothing is recomputed:** there's no new model call, it's instant and it costs nothing. The verdict, rubric scores, interviewer signals, call stats and the room's timeline are exactly as they were.
- **A new version appears only when an input changes:** another model, a different transcript (for example after Swap Speakers), or a new prompt version.
- **Jev's verdicts** on each interviewer turn and each answer are stored the same way, so the same turn always gets the same verdict. Jev isn't pinned to a version: each version records which Jev answered.

This is consistency through storage. Asking a model again would give a different answer, so an unchanged report simply isn't asked again.

## Choose the model, or the cheapest

The report's **Re-run** menu now offers:
- **Same model**, which gives the same report back if nothing changed.
- **Cheapest available**, by list price.
- **Every model your accounts can use**, each with roughly what a report costs at list price. These are your Claude models, plus OpenAI models when you've added an OpenAI key.

Prices come from LiteLLM's price list, for a typical report of about 15k tokens in and 8k out. Claude through your sign-in may be billed to your plan instead, so treat them as a guide. Going back to a model you've used before brings its version back, again at no cost.

In Terminal: `ic models` lists them, and `ic run report <id> --model <model>` or `--model cheapest` re-runs.

## Every version, in a tree

Each report page has a **Versions** section at the end:
- each transcript the reports were built on (transcribed, speakers swapped by you, or swapped by the analysis);
- the versions built on each, with model, date, verdict, your share of talk time, the room's temperature, signals, and the average rubric score;
- what changed from the version it was re-run from;
- how many unchanged re-runs showed it again;
- the next steps planned from it.

A line under the title says which version you're reading and whether it's the current one. Click any version to open it; in the app, the report pane switches to it. The report menu is now **Versions (n)**. Every transcript is now kept, where before a speaker swap replaced the old one.

## Smaller changes

- **Speaker swaps happen once.** On one-track recordings, the analysis can still swap You and Interviewer, but only on a fresh transcript. If another model later disagrees, it adds a warning suggesting Swap Speakers rather than flipping the transcript back.
- **The company field stays as you entered it.** The company a report infers is no longer written back into the interview; lists still show it. Writing it back changed the next report's inputs.
- **Old reports:** reports from before this version show "Made before versions recorded their inputs" and can't be reused, so re-running them makes a new version.
- **Next steps** aren't reused: each run plans for today's date.

## Validation

- **Tests:** 121 Rust unit tests, 21 analysis and stage tests, and 32 Swift tests pass, and clippy is clean.
- **Live, on a copy of a real interview:**
  - re-running with Claude Haiku 4.5 took 70 seconds and made version 2;
  - the same command again returned version 2 in 0.02 seconds, with no model call;
  - `--model cheapest` picked Haiku and also returned version 2;
  - the transcript wasn't flipped.
- **Every model:** all 13 Claude models this sign-in can use accepted the app's request settings, each tested with one tiny request. OpenAI models weren't tested, because there's no OpenAI key on this Mac.
- **Report page:** checked in a browser. In the app, version links were checked with a unit test, not by clicking.
