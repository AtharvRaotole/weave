"""Headless smoke test for the playground.

    pip install playwright && playwright install chromium
    python playground/tests/smoke.py http://localhost:8765/ [screenshot_dir]
"""
import sys

from playwright.sync_api import sync_playwright, expect

URL = sys.argv[1] if len(sys.argv) > 1 else "http://localhost:8765/"
SHOTS = sys.argv[2] if len(sys.argv) > 2 else None
EXPECT_ROWS = {"fanout": 5, "chain": 5, "triage": 3, "writes": 5, "errors": 3}


def main():
    errors = []
    with sync_playwright() as p:
        browser = p.chromium.launch()
        page = browser.new_page(viewport={"width": 1400, "height": 1000})
        page.on("console", lambda m: m.type == "error" and errors.append(m.text))
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.goto(URL)
        status = page.locator("#status")
        expect(status).to_contain_text("Done", timeout=20000)
        expect(page.locator("#timeline .row")).to_have_count(5)
        expect(page.locator("#check")).to_contain_text("Passed")
        expect(page.locator("#output .stdout")).to_contain_text("warmest: Mumbai at 31C")
        if SHOTS:
            page.wait_for_timeout(1800)
            page.screenshot(path=f"{SHOTS}/desktop-fanout.png", full_page=True)

        for pid, rows in EXPECT_ROWS.items():
            page.click(f'#presets button[data-id="{pid}"]')
            expect(status).to_contain_text("Done")
            expect(page.locator("#timeline .row")).to_have_count(rows)
        writes = page.locator("#effects li")
        page.click('#presets button[data-id="writes"]')
        expect(writes).to_have_count(2)
        expect(page.locator("#output .stdout")).to_contain_text("visits: 41 -> 42")
        plan = page.locator("#plan")
        expect(plan).to_contain_text("plan: 5 tool calls")
        expect(plan).to_contain_text("writes db:visits")
        expect(page.locator("#plan .plan-write")).to_have_count(2)

        page.click('#presets button[data-id="check"]')
        expect(status).to_contain_text("Rejected by the check. 0 tools called.")
        expect(page.locator("#check .diag")).to_have_count(5)
        expect(page.locator("#check")).to_contain_text("did you mean 'temp_c'?")
        expect(page.locator("#check")).to_contain_text("did you mean `weather`?")
        expect(page.locator("#check")).to_contain_text("did you mean `limit`?")
        expect(page.locator("#timeline .row")).to_have_count(0)
        if SHOTS:
            page.screenshot(path=f"{SHOTS}/desktop-check.png", full_page=True)

        # Live check while typing.
        page.click('#presets button[data-id="fanout"]')
        expect(status).to_contain_text("Done")
        page.fill("#src", 'x = weathr("Paris")\nprint(x)\n')
        expect(page.locator("#check")).to_contain_text("did you mean `weather`?", timeout=3000)
        page.fill("#src", 'print(weather("Tokyo")["sky"])\n')
        page.focus("#src")
        page.keyboard.press("Control+Enter")
        expect(page.locator("#output .stdout")).to_contain_text("clear")

        # Share link round trip.
        page.context.grant_permissions(["clipboard-read", "clipboard-write"])
        page.click("#share")
        shared = page.url
        assert "#code=" in shared, shared
        page2 = browser.new_page()
        page2.goto(shared)
        expect(page2.locator("#output .stdout")).to_contain_text("clear", timeout=20000)
        page2.close()

        # Hostile input must not wedge the page.
        page.fill("#src", "x = 0\nfor i in range(1000):\n    for j in range(1000):\n        for k in range(10):\n            x = x + 1\n")
        page.click("#run")
        expect(page.locator("#output")).to_contain_text("step limit", timeout=20000)

        mobile = browser.new_page(viewport={"width": 390, "height": 844}, is_mobile=True, has_touch=True)
        mobile.goto(URL + "#triage")
        expect(mobile.locator("#status")).to_contain_text("Done", timeout=20000)
        width = mobile.evaluate("document.documentElement.scrollWidth")
        assert width <= 392, f"horizontal overflow on mobile: {width}px"
        if SHOTS:
            mobile.wait_for_timeout(1800)
            mobile.screenshot(path=f"{SHOTS}/mobile-triage.png", full_page=True)
        browser.close()
    assert not errors, errors
    print("playground smoke test passed:", URL)


if __name__ == "__main__":
    main()
