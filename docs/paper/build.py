"""Build docs/paper/weave.pdf from weave.md (needs `pip install markdown` and Chrome or Chromium).

    python docs/paper/build.py
"""

import re
import shutil
import subprocess
import sys
from pathlib import Path

import markdown

HERE = Path(__file__).resolve().parent
CSS = """
body { font-family: 'Times New Roman', serif; font-size: 10.5pt; line-height: 1.38; max-width: 7in; margin: auto; }
h1 { font-size: 16pt; text-align: center; margin-bottom: 2pt; } .meta { text-align: center; margin-bottom: 14pt; }
h2 { font-size: 12.5pt; margin-top: 14pt; } h3 { font-size: 11pt; }
table { border-collapse: collapse; font-size: 8.5pt; margin: 6pt 0; } th, td { border-bottom: 0.5pt solid #999; padding: 2pt 5pt; }
img { max-width: 100%; } code { font-size: 9pt; } pre { background: #f5f5f5; padding: 6pt; font-size: 8.5pt; white-space: pre-wrap; }
@page { size: letter; margin: 0.8in; }
"""


def main():
    text = (HERE / "weave.md").read_text()
    front = dict(re.findall(r'^(\w+): "(.*)"$', text.split("---")[1], re.M))
    body = text.split("---", 2)[2]
    body = body.replace("../../assets/", str(HERE.parents[1] / "assets") + "/")
    html = markdown.markdown(body, extensions=["tables", "fenced_code"])
    page = (f"<html><head><meta charset='utf-8'><style>{CSS}</style></head><body><h1>{front['title']}</h1>"
            f"<div class='meta'>{front['author']}<br>{front['date']}</div>{html}</body></html>")
    (HERE / "weave.html").write_text(page)
    chrome = next((c for c in ("google-chrome", "chromium", "chromium-browser") if shutil.which(c)), None)
    if not chrome:
        sys.exit("no Chrome/Chromium found; wrote weave.html only")
    subprocess.run([chrome, "--headless", "--no-sandbox", "--disable-gpu", "--no-pdf-header-footer",
                    f"--print-to-pdf={HERE / 'weave.pdf'}", str(HERE / "weave.html")], check=True,
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    (HERE / "weave.html").unlink()
    print("wrote", HERE / "weave.pdf")


if __name__ == "__main__":
    main()
