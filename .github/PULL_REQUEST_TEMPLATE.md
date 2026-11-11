## What and why

<!-- One paragraph. Link the issue or the trace that motivated it. -->

## Checks

- [ ] `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Python tests that touch this area (`tests/python`, `tests/audit`, `bench/v4/tests`)
- [ ] New behaviour has a regression test; language changes update `docs/language.html`
- [ ] Effects, replay and the static check stay sound (say how if this touches the scheduler)
- [ ] Benchmark changes: no tuning on held-out/confirm suites; results come with `run.json` and `rows.jsonl`
- [ ] `CHANGELOG.md` updated under Unreleased
