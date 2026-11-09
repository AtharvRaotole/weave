# Releasing

## Cutting a release

1. Move the `CHANGELOG.md` entries from `[Unreleased]` to a dated version heading,
   and make sure the version in `Cargo.toml` and `pyproject.toml` matches.
2. Merge to `main` and wait for CI to pass.
3. Tag and push: `git tag -a vX.Y.Z -m "Weave X.Y.Z" && git push origin vX.Y.Z`.
4. `release.yml` builds wheels (Linux x86_64 and aarch64, macOS arm64 and x86_64,
   Windows x64, Python 3.9–3.13) and an sdist, and uploads them as run artifacts.
5. Create the GitHub Release from the tag with the changelog section as notes,
   and attach the wheels and sdist (`gh run download <run-id> -D dist`).

## Publishing to PyPI (off by default)

The `pypi` job in `release.yml` uses PyPI
[trusted publishing](https://docs.pypi.org/trusted-publishers/), so no API token
is stored in the repo. It runs only for a `v*` tag and only when the repository
variable `PYPI_PUBLISH` is `true`.

**The name `weave` is taken on PyPI** (it is Weights & Biases' Weave, which also
imports as `weave`). Since 1.0.0 the distribution name in `pyproject.toml` is
`weave-lang` (free on PyPI on 2026-11-09; not reserved until the first upload). The import name can stay `weave`, but it
will clash with W&B's package if both are installed.

One-time setup, done by the repository owner:

1. On pypi.org, sign in and open **Your account → Publishing → Add a new pending
   publisher → GitHub**, then enter:
   - PyPI project name: the distribution name from `pyproject.toml`
   - Owner: `AtharvRaotole`
   - Repository name: `weave`
   - Workflow name: `release.yml`
   - Environment name: `pypi`
2. On GitHub, open **Settings → Environments → New environment**, name it `pypi`,
   and optionally add yourself as a required reviewer so each upload waits for
   approval.
3. Open **Settings → Secrets and variables → Actions → Variables** and add
   `PYPI_PUBLISH` with the value `true`.
4. Push the next version tag. The `pypi` job uploads the wheels and sdist from
   that run. To try it first, register the same publisher on
   [test.pypi.org](https://test.pypi.org) and add
   `repository-url: https://test.pypi.org/legacy/` to the publish step.

PyPI never accepts the same version twice, so a failed or wrong upload means
bumping the version.
