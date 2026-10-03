# Interview Coach 0.1.0 preview 4 — A layout where nothing gets cut off

## The window

- **The toolbar always fits:** Record (or Stop, with the timer) on the left, and Import, Outcome, Show in Finder and Setup on the right.
  - **What went wrong:** while something was running, macOS used to hide every toolbar button behind a `»` chevron, and errors appeared as a bare red ✗.
- **Messages get their own banner** under the toolbar, with room for the whole sentence:
  - what's running;
  - errors, which you can now select, copy and dismiss;
  - a live recording problem;
  - setup that's still unfinished.
- **The stage cards** show short titles (Recording, Transcript, Report, Next steps), so they no longer truncate. When each stage ran is in its header.
- **Stage buttons** sit beside the title when there's room, and move under it when there isn't; their text is never cut off.
  - The report's run picker is now a compact **Runs** menu.
- **The menu-bar panel** keeps Open and Import, and moves Setup, Check for Updates and Quit into a `⋯` menu.

## Behind the scenes, for the Jev comparison

- **Comparison groundwork:** the typed Jev client, per-answer checks, and the scorer comparison are in this release, but answer-by-answer checks stay off until the comparison has run and picked a scorer. The rule is in `docs/eval/scorer-decision.md`, and was written before any results.
- **Smaller Claude models:** Claude Haiku 4.5 requests now leave out options that model rejects (adaptive thinking, effort, fallbacks).

## Validation

- **Tests:** 87 Rust unit tests, 17 analysis and stage tests, and 31 Swift tests pass, and clippy is clean.
- **Layout:** checked by rendering the app's own views offscreen in a development build (`IC_SNAPSHOTS`; nothing on screen is captured):
  - the window at 820 and 1100 points wide, idle, working, recording and showing an error;
  - each stage at 540, 760 and 1000 points;
  - the menu-bar panel and the Setup window.

  No button text was cut off.

## Known limits

- The recorder fix from preview 2 (switching headphones mid-call) is still waiting on a real-call test.
