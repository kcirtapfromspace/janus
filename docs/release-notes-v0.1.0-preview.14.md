# Janus 0.1.0-preview.14

- Record the call's video along with the audio. Janus records only the call's window (Zoom, Teams, Webex, FaceTime, Slack, or a Meet, Teams or Zoom browser tab), never the rest of your screen. Turn it on or off in the Record window.
- Video needs Screen Recording permission: allow it in Setup or the Record window, then reopen Janus. Without it, interviews record audio only, as before. Janus never shows the permission prompt as an interview starts.
- Watch the video with the audio on the Recording, Transcript and Review tabs. Clicking a quote's timestamp shows how people looked when it was said.
- Experimental: the review's **What the video showed** counts, for each of your answers, how many others were on camera, their nods, and how often they looked away. Faces are read on this Mac with Apple's Vision framework: positions and head angles only, never expressions, emotions or identities, and nothing is sent anywhere. These counts aren't yet validated on real calls.
- The consent confirmation names video when it's being recorded. From the terminal, `ic record --video` records it too.

Existing interviews, settings, sign-ins, and automatic updates carry forward. Video starts with interviews recorded after this update. Requires macOS 14.4 or later on Apple silicon.
