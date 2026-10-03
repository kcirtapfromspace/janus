# Interview Coach 0.1.0 preview 7 — Switch accounts and restart the proxy from Setup

Setup used to offer buttons only for things still to do. Once a row was done, changing it meant using Terminal. Now every finished row you might want to change has a button:

- **Signed in to Claude as you@example.com (Your Organization).** The row shows which account the app uses. **Switch Account…** signs this app out, then asks you to approve access in your browser again.
  - Sign-in approves whichever account your browser is signed in to at claude.ai, so switch there first.
  - Other apps keep their own Claude sign-in.
- **AI proxy is running → Restart.** Stops and starts the proxy. Your keys and spend history are kept.
- **TypeSafe or OpenAI key → Replace**, as before.

Switch Account…, Restart and Replace wait while the app is transcribing or analysing, because signing out or restarting the proxy mid-step would make that step fail.

The same actions are available in Terminal: `ic login --switch`, `ic logout` and `ic setup run restart-proxy`.

## Validation

- **Tests:** 116 Rust unit tests, 18 analysis and stage tests, and 30 Swift tests pass, and clippy is clean.
- **Restart:** run on this Mac's real proxy. It was back in 19 seconds, and Jev answered through it straight after.
- **Sign-out:** run against a copy of the sign-in settings. It removed only Interview Coach's profile. Other profiles, and the default other tools use, were untouched.
- **Setup window:** checked with the offscreen renderer.
- **Not yet done:** the browser part of switching accounts hasn't been run end to end, because it needs you to approve access.
