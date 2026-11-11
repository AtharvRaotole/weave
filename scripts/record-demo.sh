#!/bin/bash
# Re-record the README demo from a real run of examples/demo.py.
# Needs: asciinema (pip), node (npx svg-term-cli), google-chrome (headless) and Pillow.
set -euo pipefail
cd "$(dirname "$0")/.."
asciinema rec -q --overwrite --cols 92 --rows 28 -c scripts/demo-session.sh assets/demo.cast
npx -y svg-term-cli --in assets/demo.cast --out assets/demo.svg --window --width 92 --height 28 --padding 12
python3 scripts/demo_gif.py assets/demo.cast assets/demo.gif
