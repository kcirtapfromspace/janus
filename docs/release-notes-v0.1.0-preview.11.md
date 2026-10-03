# Interview Coach 0.1.0-preview.11

- Sign in with ChatGPT, manage saved accounts, and select available coaching models in Setup. ChatGPT plan usage requires account permission; adding an OpenAI API key explicitly selects separate API billing.
- Jev is now required for answer-quality and interviewer-signal evaluations. Add a TypeSafe key in Setup. Legacy disabled or Claude scorer settings no longer disable Jev in the standard analysis workflow.
- Coaching and evaluation use native provider APIs without Docker. Existing local keys remain supported.
- Evaluation failures preserve recordings, transcripts, coaching, and completed checks so retrying can finish the report.
- Credentials use private local storage, serialized token refresh, and sign-out revocation.

This remains a preview. Live account eligibility and provider behavior still need end-to-end verification.
