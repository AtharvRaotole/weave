#!/bin/sh
# Install Weave from the prebuilt wheels attached to a GitHub release (no Rust toolchain needed).
#
#   curl -fsSL https://raw.githubusercontent.com/AtharvRaotole/weave/main/scripts/install.sh | sh
#   curl -fsSL .../install.sh | sh -s -- --mcp        # also build the weave-mcp server (needs cargo)
#
# Environment:
#   WEAVE_VERSION      release tag to install (default: the latest release, e.g. v1.1.0)
#   PYTHON             interpreter to install into (default: python3, or python)
#   WEAVE_FIND_LINKS   a local directory or URL with wheels instead of the release page (used by CI)
set -eu

REPO="AtharvRaotole/weave"
WANT_MCP=0
for arg in "$@"; do
  case "$arg" in
    --mcp) WANT_MCP=1 ;;
    -h|--help) sed -n '2,11p' "$0" 2>/dev/null || true; exit 0 ;;
    *) echo "install.sh: unknown argument $arg" >&2; exit 2 ;;
  esac
done

PY="${PYTHON:-}"
if [ -z "$PY" ]; then
  if command -v python3 >/dev/null 2>&1; then PY=python3; elif command -v python >/dev/null 2>&1; then PY=python
  else echo "install.sh: Python 3.9-3.13 is required" >&2; exit 1; fi
fi
"$PY" -c 'import sys; sys.exit(0 if (3, 9) <= sys.version_info[:2] <= (3, 13) else 1)' || {
  echo "install.sh: $PY is not Python 3.9-3.13 (prebuilt wheels cover cp39-cp313)" >&2; exit 1; }

if [ -n "${WEAVE_FIND_LINKS:-}" ]; then
  LINKS="$WEAVE_FIND_LINKS"; SPEC="weave-lang"
else
  TAG="${WEAVE_VERSION:-}"
  if [ -z "$TAG" ]; then
    TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" | "$PY" -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')
  fi
  LINKS="https://github.com/$REPO/releases/expanded_assets/$TAG"; SPEC="weave-lang==${TAG#v}"
fi

"$PY" -m pip --version >/dev/null 2>&1 || "$PY" -m ensurepip --upgrade >/dev/null 2>&1 || {
  echo "install.sh: pip is not available for $PY" >&2; exit 1; }
echo "Installing $SPEC from $LINKS into $("$PY" -c 'import sys; print(sys.executable)')"
"$PY" -m pip install --upgrade --only-binary weave-lang --find-links "$LINKS" "$SPEC"
"$PY" -c 'import weave; e = weave.Engine(); print("weave", weave.__version__ if hasattr(weave, "__version__") else "", "ok")'

if [ "$WANT_MCP" = 1 ]; then
  command -v cargo >/dev/null 2>&1 || { echo "install.sh: --mcp needs cargo (https://rustup.rs)" >&2; exit 1; }
  cargo install --locked --git "https://github.com/$REPO" ${TAG:+--tag "$TAG"} weave-mcp
  echo "weave-mcp installed. Add it to Claude Desktop or Cursor with: weave mcp-config --config tools.json --write"
fi
