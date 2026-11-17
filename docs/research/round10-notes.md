# Round 10 notes: the write gate

**Problem (round 9 traces).** With GPT-5.4 mini at effort `none`, Weave lost BFCL tasks when a script batched writes
on values the model never saw: an item picked from a listing, or a step that ran after a call had already failed.
Native function calling cannot do this, because the model sees each result before its next call.

**Idea.** Restore that guarantee only at writes. A write is control- or data-dependent on unseen results when either:
(a) an earlier call of the script failed, or (b) an argument value was picked from a list the model has not seen.
Values count as seen when they appear in the model's script, the tool's earlier outputs, or the user's messages, so
the check needs no interpreter changes. It works by value matching, like taint tracking by value.

**Variants ablated on dev (train ids):** `flow` (any unseen-derived value), `choice` (picked from a list), and
`errors` (failures only; implemented but not run for lack of budget). `flow` paused on harmless lookups and scored
lower; `choice` scored best (24/40 vs W3's 22/40, within noise).

**Outcome.** See paper §5.18. GPT-5.4 mini scored +6 tasks over native on 105 fresh pairs (not significant) at equal
cost. GPT-4.1 scored 3/16 vs 5/16.

**Not built, and the next lever for cost.** Weave's input is about 20 % smaller, but its cache share is lower and it
writes more output. A stable prefix (the description first, nothing varying before it) and shorter scripts would
turn the token saving into a dollar saving.
