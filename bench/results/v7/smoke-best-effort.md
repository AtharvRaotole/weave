# Round 4 best-effort providers (Addendum 4: one attempt, only if a smoke request answers)

Smoke request: one `Reply with OK` chat completion, max_tokens 16, 35 s timeout, sent 2026-11-10 23:17 ET.

| provider | model | result | confirm4 run |
|---|---|---|---|
| nvidia | z-ai/glm-5.3-flash | timeout after 35.0 s (fourth consecutive timeout since round 3) | not run |
| gemini | gemini-3.5-flash | HTTP 429 "You exceeded your current quota" after 0.2 s | not run |
