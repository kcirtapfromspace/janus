# Janus 0.1.0-preview.15

- New tools for checking the experimental video cues against what people actually see. `ic eval clips` cuts interviews recorded with video into 20-second clips. `ic eval label` opens a page on this Mac for labelling each one: nods, looking away, smiles, and who's on camera. Labels stay in `~/InterviewCoach/eval/video`, never in the app's reviews.
- The test is written down before any results, in `docs/eval/video-decision.md` (the same approach as Jev's checks). The review's **What the video showed** stays marked experimental until its cues pass.

The app itself is unchanged. Existing interviews, settings, sign-ins, and automatic updates carry forward. Requires macOS 14.4 or later on Apple silicon.
