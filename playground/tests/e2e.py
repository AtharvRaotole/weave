"""End-to-end test for the playground, in headless Chromium.

    pip install playwright && playwright install chromium
    python playground/tests/e2e.py http://localhost:8765/ [screenshot_dir]

Works against a local build or the live site. Prints one line per check and
exits non-zero if any check fails.
"""
import re
import sys
import traceback

from playwright.sync_api import sync_playwright, expect

URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8765/"
SHOTS = sys.argv[2] if len(sys.argv) > 2 else None
# Timeline rows each example should produce.
EXPECT_ROWS = {"morning": 5, "fanout": 5, "chain": 5, "triage": 3, "writes": 5, "check": 0, "errors": 3}
TEXT_FILES = ["", "index.html", "app.js", "presets.js", "style.css"]

results = []


def check(name):
    def wrap(fn):
        try:
            fn()
            results.append((name, True, ""))
        except Exception as e:  # noqa: BLE001 - we report every failure
            msg = str(e).strip().splitlines()[0] if str(e).strip() else type(e).__name__
            results.append((name, False, msg[:300]))
            if "-v" in sys.argv:
                traceback.print_exc()
        return fn
    return wrap


def main():
    errors, bad_requests = [], []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        ctx = browser.new_context(viewport={"width": 1280, "height": 800})
        ctx.grant_permissions(["clipboard-read", "clipboard-write"])
        page = ctx.new_page()
        page.on("console", lambda m: m.type == "error" and errors.append(m.text))
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on("requestfailed", lambda r: bad_requests.append(f"{r.url} {r.failure}"))
        page.on("response", lambda r: r.status >= 400 and bad_requests.append(f"{r.url} {r.status}"))
        wasm = []
        page.on("response", lambda r: r.url.endswith(".wasm") and wasm.append(r.status))
        page.goto(URL)
        status = page.locator("#status")
        explain = page.locator("#explain")

        @check("wasm loads and the default example runs on its own")
        def _():
            expect(status).to_contain_text("Done", timeout=20000)
            assert wasm and wasm[0] == 200, wasm
            expect(page.locator("#ver")).to_contain_text("Runtime v")

        @check("fonts load (Inter, JetBrains Mono)")
        def _():
            page.evaluate("document.fonts.ready")
            ok = page.evaluate("[document.fonts.check('14px Inter'), document.fonts.check('13px \"JetBrains Mono\"')]")
            assert ok == [True, True], ok
            loaded = page.evaluate("[...document.fonts].filter(f => f.status === 'loaded').map(f => f.family)")
            assert any("Inter" in f for f in loaded) and any("JetBrains" in f for f in loaded), loaded

        @check("hero says what Weave is, with install and GitHub links")
        def _():
            expect(page.locator(".hero h1")).to_contain_text("one short script instead of a dozen tool calls")
            expect(page.locator(".hero .lede")).to_contain_text("Python-like language")
            expect(page.locator(".hero [data-copy]")).to_contain_text("pip install weave-lang")
            expect(page.locator(".hero a.gh-link")).to_have_attribute("href", "https://github.com/AtharvRaotole/weave")
            expect(page.locator(".steps li")).to_have_count(3)

        @check("default example: task, check, plan with its write, timeline, explanation")
        def _():
            expect(page.locator('#presets button[aria-selected="true"]')).to_have_attribute("data-id", "morning")
            expect(page.locator("#task")).to_contain_text("The task:")
            expect(page.locator("#check")).to_contain_text("Looks good")
            expect(page.locator("#plan li")).to_have_count(3)
            expect(page.locator("#plan .plan-write")).to_have_count(1)
            expect(page.locator("#plan .plan-write")).to_contain_text("post_slack")
            expect(page.locator("#timeline .row")).to_have_count(5)
            expect(explain).to_contain_text("up to 4 at the same time")
            expect(explain).to_contain_text("250 ms instead of 790 ms")
            expect(page.locator("#effects li")).to_have_count(1)
            expect(page.locator("#output .stdout")).to_contain_text("Morning! London 11C rain")
            if SHOTS:
                page.wait_for_timeout(1800)
                page.screenshot(path=f"{SHOTS}/desktop.png")
                page.screenshot(path=f"{SHOTS}/desktop-full.png", full_page=True)

        @check("the fake tools are listed (open on desktop)")
        def _():
            expect(page.locator("#tools-box")).to_have_attribute("open", "")
            expect(page.locator("#tools .tool")).to_have_count(8)

        for pid, rows in EXPECT_ROWS.items():
            @check(f"example '{pid}' loads, runs and explains itself")
            def _(pid=pid, rows=rows):
                page.click(f'#presets button[data-id="{pid}"]')
                expect(page.locator('#presets button[aria-selected="true"]')).to_have_attribute("data-id", pid)
                expect(page.locator("#task")).not_to_be_empty()
                if pid == "check":
                    expect(status).to_contain_text("Rejected by the check. 0 tools called.")
                else:
                    expect(status).to_contain_text("Done")
                expect(page.locator("#timeline .row")).to_have_count(rows)
                expect(explain).to_be_visible()
                assert page.url.endswith("#" + pid), page.url

        @check("plan lists every write before running (writes example)")
        def _():
            page.click('#presets button[data-id="writes"]')
            expect(page.locator("#plan li")).to_have_count(5)
            expect(page.locator("#plan-box .chip.write")).to_contain_text("2 writes")
            expect(page.locator("#plan")).to_contain_text("writes db:visits")
            expect(page.locator("#plan .plan-write")).to_have_count(2)
            expect(page.locator("#effects li")).to_have_count(2)
            expect(page.locator("#output .stdout")).to_contain_text("visits: 41 -> 42")

        @check("broken script: readable errors with hints, nothing runs")
        def _():
            page.click('#presets button[data-id="check"]')
            expect(page.locator("#check .diag")).to_have_count(5)
            expect(page.locator("#check")).to_contain_text("did you mean 'temp_c'?")
            expect(page.locator("#check")).to_contain_text("did you mean `weather`?")
            expect(page.locator("#check")).to_contain_text("did you mean `limit`?")
            expect(explain).to_contain_text("no tool was called")
            expect(page.locator("#plan-box")).to_contain_text("Fix what the check found")
            expect(page.locator("#effects")).to_contain_text("Nothing was written")
            if SHOTS:
                page.locator("#check-card").scroll_into_view_if_needed()
                page.screenshot(path=f"{SHOTS}/desktop-check.png")

        @check("clicking a problem jumps to its line")
        def _():
            page.locator("#check .diag").first.click()
            pos = page.evaluate("(() => { const s = document.getElementById('src'); return s.value.slice(0, s.selectionStart).split('\\n').length; })()")
            assert pos == 3, pos

        @check("caught error: the run carries on and says so")
        def _():
            page.click('#presets button[data-id="errors"]')
            expect(status).to_contain_text("Done")
            expect(page.locator("#timeline .barx.fail")).to_have_count(1)
            expect(explain).to_contain_text("caught it")
            expect(page.locator("#output .stdout")).to_contain_text("got 2 of 3")

        @check("uncaught error: readable message, run stops")
        def _():
            page.fill("#src", 'x = weather("Atlantis")\nprint(x)\n')
            page.click("#run")
            expect(status).to_contain_text("Stopped on an error")
            expect(page.locator("#output .diag")).to_contain_text("Atlantis")
            expect(explain).to_contain_text("nothing caught it")
            expect(page.locator("#timeline .barx.fail")).to_have_count(1)

        @check("edits re-check as you type and re-run with Ctrl+Enter")
        def _():
            page.fill("#src", 'x = weathr("Paris")\nprint(x)\n')
            expect(page.locator("#check")).to_contain_text("did you mean `weather`?", timeout=3000)
            page.fill("#src", 'print(weather("Tokyo")["sky"])\n')
            expect(page.locator("#check")).to_contain_text("Looks good", timeout=3000)
            page.focus("#src")
            page.keyboard.press("Control+Enter")
            expect(page.locator("#output .stdout")).to_contain_text("clear")
            expect(page.locator("#timeline .row")).to_have_count(1)

        @check("share copies a link that opens the same script")
        def _():
            page.click("#share")
            shared = page.url
            assert "#code=" in shared, shared
            clip = page.evaluate("navigator.clipboard.readText()")
            assert clip == shared, (clip, shared)
            page2 = ctx.new_page()
            page2.goto(shared)
            expect(page2.locator("#output .stdout")).to_contain_text("clear", timeout=20000)
            expect(page2.locator("#blurb")).to_contain_text("shared with you")
            page2.close()

        @check("install button copies the pip command")
        def _():
            page.locator(".hero [data-copy]").click()
            assert page.evaluate("navigator.clipboard.readText()") == "pip install weave-lang"
            expect(page.locator(".toast")).to_contain_text("pip install weave-lang")

        @check("timeline rows open details, replay works")
        def _():
            page.click('#presets button[data-id="morning"]')
            expect(status).to_contain_text("Done")
            page.locator("#timeline .row").first.click()
            expect(page.locator("#timeline .detail")).to_contain_text("result")
            page.click("#replay")
            expect(page.locator("#timeline .row")).to_have_count(5)

        @check("reset puts the example back")
        def _():
            page.fill("#src", "print(1)\n")
            page.click("#reset")
            expect(page.locator("#src")).to_have_value(re.compile(r"post_slack"))

        @check("step 3 button runs and scrolls to the timeline")
        def _():
            page.evaluate("window.scrollTo(0, 0)")
            page.locator('.steps button[data-go="timeline-card"]').click()
            page.wait_for_timeout(900)
            top = page.evaluate("document.getElementById('timeline-card').getBoundingClientRect().top")
            assert 0 <= top < 200, top
            expect(status).to_contain_text("Done")

        @check("a runaway loop hits the step limit instead of hanging")
        def _():
            page.fill("#src", "x = 0\nfor i in range(1000):\n    for j in range(1000):\n        for k in range(10):\n            x = x + 1\n")
            page.click("#run")
            expect(page.locator("#output")).to_contain_text("step limit", timeout=20000)

        @check("no em or en dashes in the site")
        def _():
            base = URL.split("#")[0]
            bad = []
            for f in TEXT_FILES:
                body = page.request.get(base + f).text()
                if "\u2014" in body or "\u2013" in body:
                    bad.append(f or "/")
            assert not bad, bad

        @check("benchmark numbers, if any, keep the BFCL loss")
        def _():
            text = page.inner_text("body")
            if "BFCL" in text:
                assert "18/60" in text and "27/60" in text

        @check("deep link to an example (#triage)")
        def _():
            p3 = ctx.new_page()
            p3.goto(URL.split("#")[0] + "#triage")
            expect(p3.locator('#presets button[aria-selected="true"]')).to_have_attribute("data-id", "triage", timeout=20000)
            expect(p3.locator("#status")).to_contain_text("Done", timeout=20000)
            expect(p3.locator("#timeline .row")).to_have_count(3)
            p3.close()

        mobile = browser.new_page(viewport={"width": 390, "height": 844}, is_mobile=True, has_touch=True, device_scale_factor=2)
        mobile.on("console", lambda m: m.type == "error" and errors.append(m.text))
        mobile.on("pageerror", lambda e: errors.append(str(e)))

        @check("mobile: no sideways scroll, hero and install visible, Run works by tap")
        def _():
            mobile.goto(URL.split("#")[0])
            expect(mobile.locator("#status")).to_contain_text("Done", timeout=20000)
            width = mobile.evaluate("document.documentElement.scrollWidth")
            assert width <= 392, f"horizontal overflow on mobile: {width}px"
            expect(mobile.locator(".hero h1")).to_be_visible()
            expect(mobile.locator(".hero [data-copy]")).to_be_visible()
            expect(mobile.locator(".steps")).to_be_visible()
            if SHOTS:
                mobile.wait_for_timeout(1500)
                mobile.screenshot(path=f"{SHOTS}/mobile.png")
                mobile.screenshot(path=f"{SHOTS}/mobile-full.png", full_page=True)
            mobile.click('#presets button[data-id="triage"]')
            mobile.locator("#run").tap()
            expect(mobile.locator("#status")).to_contain_text("Done")
            expect(mobile.locator("#timeline .row")).to_have_count(3)
            width = mobile.evaluate("document.documentElement.scrollWidth")
            assert width <= 392, f"horizontal overflow after run: {width}px"

        @check("no console errors and no failed requests")
        def _():
            assert not errors, errors
            assert not bad_requests, bad_requests

        browser.close()

    w = max(len(n) for n, _, _ in results)
    for name, ok, msg in results:
        print(f"{'PASS' if ok else 'FAIL'}  {name.ljust(w)}  {msg}")
    failed = [n for n, ok, _ in results if not ok]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed against {URL}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
