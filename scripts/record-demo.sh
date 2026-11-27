#!/bin/bash
# Re-record the README demos from real runs of examples/demo.py and examples/plan_preview.py.
# Needs: asciinema (pip), node (npx svg-term-cli), google-chrome (headless) and Pillow.
set -euo pipefail
cd "$(dirname "$0")/.."
asciinema rec -q --overwrite --cols 92 --rows 28 -c scripts/demo-session.sh assets/demo.cast
npx -y svg-term-cli --in assets/demo.cast --out assets/demo.svg --window --width 92 --height 28 --padding 12
python3 scripts/demo_gif.py assets/demo.cast assets/demo.gif
# The plan preview recording (wider terminal for the plan lines).
asciinema rec -q --overwrite --cols 112 --rows 30 -c scripts/plan-session.sh assets/plan.cast
python3 scripts/demo_gif.py assets/plan.cast assets/plan.gif 112 30
