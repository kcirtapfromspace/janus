# Janus 0.1.0-preview.18

- **Know which version is running.** About Janus and Settings show the full release version, including the preview number, and the build number. About also links to the [Janus source repository](https://github.com/kcirtapfromspace/janus).
- **Choose what to record.** When recording video, use **Choose source…** to select a window, app or whole screen with the macOS picker. To record one browser tab, move it into its own window and choose that window.
- **Optional screen fallback.** Automatic meeting detection can fall back to recording your main screen when no meeting window is detected. Enable this per recording; it records everything visible on that screen.
- **Better meeting detection.** Helium browser calls are recognized. Ordinary Slack workspace and channel windows are no longer mistaken for calls, and an ended Slack huddle no longer holds capture away from a browser meeting.
- **Clearer recording warnings.** Video capture failures are visible during recording. If automatic detection cannot find a meeting, the warning explains that detection failed rather than claiming no call was open. Stopping sharing with macOS stops the selected video capture.

Existing interviews, settings, sign-ins, and automatic updates carry forward. Requires macOS 14.4 or later on Apple silicon.
