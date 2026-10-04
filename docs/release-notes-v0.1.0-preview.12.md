# Interview Coach 0.1.0-preview.12

- Fix ChatGPT stage reruns failing with “Input must be a list”: Responses requests now send message-list input.
- Stop safely while the microphone permission prompt is open. A late permission response cannot restart a cancelled recording.
- Prevent duplicate recording starts during preparation and competing recorders from claiming the same session folder.
- Preserve captured audio and report save failures clearly.
- Cancelled recording self-tests no longer play their delayed sound.

Automated payload and recording lifecycle regression tests pass. Live ChatGPT inference remains unverified on the build Mac because active ChatGPT plan credentials are unavailable.
