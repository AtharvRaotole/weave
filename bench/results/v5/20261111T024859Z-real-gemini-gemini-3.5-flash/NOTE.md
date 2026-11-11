# Not run: Gemini free-tier daily quota

Started 2026-11-10 21:48 ET with 1 worker and 12 s between requests. No trial finished: every request got
HTTP 429 "Generate content requests per day limit exceeded on Free Tier" (limit 20 requests per day; the
quota message named `gemini-3.6-flash`), and the run was stopped at about 21:59 ET. A complete `real` run needs
about 40-50 requests, more than one day's free quota. No rows, so nothing is reported for Gemini in round 2.
