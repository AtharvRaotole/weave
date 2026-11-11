"""Render an asciinema cast to a GIF: svg-term frames every 200 ms, screenshotted by headless Chrome."""
import json
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image

cast, out = Path(sys.argv[1]), Path(sys.argv[2])
end = json.loads(cast.read_text().splitlines()[-1])[0]
frames = []
with tempfile.TemporaryDirectory() as tmp:
    for i in range(int((end + 0.3) / 0.2) + 1):
        svg, png = Path(tmp) / f"{i:03d}.svg", Path(tmp) / f"{i:03d}.png"
        subprocess.run(["npx", "-y", "svg-term-cli", "--in", str(cast), "--out", str(svg), "--window", "--width", "92",
                        "--height", "28", "--padding", "12", "--at", str(int(i * 200))], check=True, capture_output=True)
        subprocess.run(["google-chrome", "--headless=new", "--disable-gpu", "--no-sandbox", "--hide-scrollbars",
                        "--window-size=944,588", f"--screenshot={png}", svg.as_uri()], check=True, capture_output=True)
        im = Image.open(png).convert("RGB")
        frames.append(im.resize((im.width * 3 // 4, im.height * 3 // 4), Image.LANCZOS).quantize(colors=64))
frames[0].save(out, save_all=True, append_images=frames[1:], duration=[200] * (len(frames) - 1) + [5000], loop=0,
               optimize=True)
