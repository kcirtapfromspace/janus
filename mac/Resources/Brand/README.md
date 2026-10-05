# Janus identity

Janus represents two perspectives on one experience: looking back to understand an interview,
and ahead to prepare for the next. The product provides evidence-based coaching when an employer
offers little feedback. It does not claim access to the employer's private decision.

The approved mark is **Roman Rhythm B**: two vertically staggered quotation marks whose outer
edges form larger, outward-facing Roman profiles. Both faces stay upright. A single editable
Bezier drawing generates the native SwiftUI shape, SVGs, report stamp, and Mac icon. Georgia Bold
supplies the wordmark; the standalone SVG lettering is outlined so it renders without an installed
font. The selected concept and its generation prompt are retained in `Concepts/Janus/`.

- `mark.svg` and `mark-dark.svg`: scalable marks with accessible titles.
- `wordmark.svg` and `wordmark-dark.svg`: horizontal Georgia Bold wordmarks.
- `app-icon.png`: transparent 1024-pixel icon preview.
- `AppIcon.icns`: native Mac app icon, copied into the signed bundle during builds.
- `report.css`: shared stylesheet for HTML exports and saved reports inside the app.
- `report-brand.html`: the shared accessible report stamp.
- `identity.png`: light and dark identity sheet with the app icon.

Regenerate the PNG and ICNS from the editable geometry with:

```sh
swift scripts/make-brand-assets.swift
```

The generator also writes `JanusMark.swift`; edit the generator rather than this generated shape.
The native components and adaptive colors live in `Brand.swift`. Use paper `#F4F0E7`,
page `#FCFAF4`, ink `#2B3029`, pencil `#6C7064`, and green `#345B49` for actions.
Dark mode keeps the same warm family. Georgia headings, system controls, and small monospaced
dates give the notebook its rhythm. Use plain rules rather than enclosing every section.
Status labels remain text, not colored pills; retain native focus and keyboard behavior.

The notebook leads with the latest conversation, its review, and earlier interviews. Facts and
recorded outcomes sit in the margin. The activity chart follows the conversation list. Avoid
promotional slogans, repeated uppercase labels, avatar initials, nested bezels, and inflated KPI
numbers. Empty states explain the next action plainly.

HTML reports use this stylesheet at generation time. The native viewer also adds it after loading
so existing reports inherit the design while keeping their original content and timestamp links.
The native viewer replaces earlier Janus stamps with the current mark when opening a saved report.

Janus is the display name and new app bundle filename. Internal bundle IDs, executable/module
names, the `ic` CLI, `IC_*` variables, existing data/cache folders, and update signing credentials
retain their established identifiers. This preserves existing interviews, preferences, sign-ins,
and the update feed. Release tooling accepts earlier Interview Coach archives too. Historical
release notes and the rejected Roundnote exploration are retained as historical records.

## Workflow and analytics

Notebook → record/import → Review → transcript evidence → Prepare. Completed interviews open
in Review; imported interviews open after processing. Capture requires explicit consent and
links to the audio check. Each interview retains its complete four-stage pipeline and report
versions. Returning to the notebook clears the interview selection and stops its audio playback.

The dashboard reads the local library and offers 30 days, 90 days, and all time. It excludes
archived/deleted interviews and dates after the current time. The day range includes today;
activity groups by local calendar week, or month for histories longer than 180 days. Zero periods
stay visible. Click or drag the activity chart to inspect a bucket's count.

Reviews count analyzed sessions or sessions with an existing report. Active roles count
unarchived interviewing roles with a session in the selected range. Conversation time sums
available durations. Recorded outcomes count the user's explicit results; missing/pending
results appear as awaiting an outcome. Predicted verdicts never count as recorded outcomes.
The dashboard is independent of sidebar search and filters.

## Visual verification

`IC_SNAPSHOTS` renders the native views into offscreen windows. The fixture mode does not run
retention cleanup, background polling, or the updater. Use an isolated `IC_DATA_DIR` for any
CLI calls. It saves overview, narrow layout, dark mode, empty state, capture, all pipeline panes,
and full windows. The report preview fixture is for layout inspection; it is not an evaluation
of coaching quality or recording hardware.
