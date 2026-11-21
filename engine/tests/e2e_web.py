"""End-to-end check of the web UI in a real browser (headless Chromium through Playwright): the
navigation, the layout on laptop and desktop windows, and dark mode. It catches what the HTTP tests
cannot see: a tab that opens /apis//calls, a column that loses its filter, a panel wider than the
window, a colour that stays light in dark mode, a colour mode choice that does not stick.

    cargo build -p stuntman && (cd web && trunk build) && python3 tests/e2e_web.py

Needs `pip install playwright && playwright install chromium`.
"""
import json, os, re, sys, time, urllib.request

from playwright.sync_api import sync_playwright

import harness

ROOT = harness.ROOT
DIST = os.path.join(ROOT, "web", "dist")
COLLECTIONS = ["Brazil · ICP-Brasil", "Payments & notifications", "Logistics & fiscal"]


def seed(s):
    """Installs a few collections and makes a handful of calls to every simple route."""
    for c in COLLECTIONS:
        s.call("POST", "/catalog/collections/install", {"name": c})
    n = 0
    for svc in s.call("GET", "/apis/services")["services"]:
        for r in s.call("GET", f"/plugins/{svc['name']}")["plugin"].get("routes", []):
            m = re.match(r"(\w+) (/\S*)$", r.get("route", ""))
            if not m:
                continue
            method, path = m.group(1), re.sub(r"\{[^}]+\}", "123", m.group(2))
            req = urllib.request.Request(f"http://127.0.0.1:{s.APIS_PORT}/{svc['name']}{path}", method=method,
                                         data=b"{}" if method in ("POST", "PUT", "PATCH") else None, headers={"content-type": "application/json"})
            try:
                urllib.request.urlopen(req, timeout=5).read()
            except Exception:
                pass
            n += 1
    return n


# Every visible tab of every service in the rail: the URL is /apis/<service>/<tab> and the rail
# keeps every service. Returns the problems found.
TABS_JS = """async () => {
  const sleep = ms => new Promise(r => setTimeout(r, ms));
  const bad = [];
  const rows = () => document.querySelectorAll('.krows .krow');
  const n = rows().length;
  for (let i = 0; i < n; i++) {
    rows()[i].click(); await sleep(120);
    const tabs = [...document.querySelectorAll('.tabs .tab')].filter(t => !t.classList.contains('hidden')).map(t => t.childNodes[0].textContent);
    for (const name of tabs) {
      const t = [...document.querySelectorAll('.tabs .tab')].find(x => x.childNodes[0].textContent === name);
      if (!t) continue;
      t.click(); await sleep(60);
      if (!/^\\/apis\\/[a-z0-9-]+\\/[a-z]+$/.test(location.pathname)) bad.push('url ' + location.pathname);
      if (rows().length !== n) bad.push('rail ' + rows().length + '/' + n + ' at ' + location.pathname);
    }
  }
  return {n, bad};
}"""

# Columns wider than their box (a horizontal scrollbar inside the page).
OVERFLOW_JS = """() => [...document.querySelectorAll('.body > *, .page > *')]
  .filter(c => c.scrollWidth > c.clientWidth + 2).map(c => location.pathname + ' ' + c.className + ' ' + c.scrollWidth + '/' + c.clientWidth)"""

# In dark mode: a light background or dark text that is not the intentional inversion (ink).
DARK_JS = """() => {
  const lum = c => { const m = c.match(/[\\d.]+/g); if (!m) return null; const [r, g, b, a] = m.map(Number); if (a !== undefined && a < 0.5) return null; return (0.2126*r + 0.7152*g + 0.0722*b) / 255; };
  const ink = getComputedStyle(document.documentElement).getPropertyValue('--ink').trim();
  const probe = document.createElement('div'); probe.style.background = ink; document.body.append(probe);
  const inkBg = getComputedStyle(probe).backgroundColor; probe.remove();
  const bad = [];
  for (const e of document.querySelectorAll('body *')) {
    const r = e.getBoundingClientRect();
    if (r.width < 8 || r.height < 8 || r.bottom < 0 || r.top > innerHeight) continue;
    if (e.closest('iframe, .wa-prev, .preview')) continue;
    const cs = getComputedStyle(e);
    let inverted = false;
    for (let p = e; p && !inverted; p = p.parentElement) inverted = getComputedStyle(p).backgroundColor === inkBg;
    const bl = lum(cs.backgroundColor);
    if (bl !== null && bl > 0.75 && cs.backgroundColor !== inkBg) bad.push('bg ' + e.tagName + '.' + e.className + ' ' + cs.backgroundColor);
    const text = [...e.childNodes].some(n => n.nodeType === 3 && n.textContent.trim());
    const tl = lum(cs.color);
    if (text && !inverted && tl !== null && tl < 0.25) bad.push('text ' + e.tagName + '.' + e.className + ' ' + JSON.stringify(e.textContent.trim().slice(0, 30)));
  }
  return [...new Set(bad)].slice(0, 8);
}"""

PAGES = ["/timeline", "/telemetry/traces", "/apis/stripe/calls", "/apis/stripe/routes", "/apis/stripe/answers", "/apis/stripe/auth",
         "/apis/stripe/settings", "/apis/vidaas/pages", "/apis/new", "/catalog", "/catalog/stripe", "/environments", "/settings/access",
         "/notifications", "/inboxes", "/queues", "/queues/subscriptions", "/files", "/files/links", "/secrets", "/identity", "/ai", "/events"]


def go(page, path):
    page.evaluate("p => { history.pushState({}, '', p); dispatchEvent(new PopStateEvent('popstate')); }", path)
    page.wait_for_timeout(500)


def main():
    if not os.path.exists(os.path.join(DIST, "index.html")):
        sys.exit("build the web UI first: (cd web && trunk build)")
    fails = []
    with harness.Stuntman(env={"STUNTMAN_WEB": DIST}) as s, sync_playwright() as pw:
        calls = seed(s)
        base = f"http://127.0.0.1:{s.PORT}"
        browser = pw.chromium.launch()
        page = browser.new_page(viewport={"width": 1440, "height": 900})
        errors = []
        page.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)
        page.on("pageerror", lambda e: errors.append(str(e)))

        # 1. Every tab of every service opens its own URL; the rail never empties.
        page.goto(f"{base}/apis")
        page.wait_for_selector(".krows .krow")
        r = page.evaluate(TABS_JS)
        print(f"tabs: {r['n']} services, {calls} calls")
        fails += r["bad"]

        # 2. The rail keeps its filter across tabs (HTTP APIs, Storage, Queues).
        for a, b, sel in [("/apis/stripe/calls", "/apis/stripe/routes", ".kcol input"), ("/files", "/files/links", ".kcol input"),
                          ("/queues", "/queues/subscriptions", ".kcol input")]:
            go(page, a)
            page.fill(sel, "zz")
            go(page, b)
            if page.input_value(sel) != "zz":
                fails.append(f"filter lost from {a} to {b}")

        # 3. The Calls range and search live in the URL: a link opens that view.
        page.goto(f"{base}/apis/stripe/calls?since=1d&q=customer")
        page.wait_for_timeout(800)
        if "Last 24 h" not in page.inner_text(".head") or page.input_value("input[placeholder^='Path']") != "customer":
            fails.append("since/q from the URL not applied")
        page.click(".head button:has-text('Last')")
        page.click(".mi:has-text('Last hour')")
        page.wait_for_timeout(300)
        if "since=1h" not in page.url:
            fails.append(f"range not written to the URL: {page.url}")

        # 4. No column wider than its box, on a laptop and a desktop window.
        for w, h in [(1280, 800), (1440, 900)]:
            page.set_viewport_size({"width": w, "height": h})
            for p in PAGES:
                go(page, p)
                fails += [f"{w}px overflow {x}" for x in page.evaluate(OVERFLOW_JS)]

        # 5. The colour mode at the foot of the rail: a choice beats the system and survives a reload;
        # System follows the system.
        theme = lambda: page.evaluate("document.documentElement.dataset.theme")
        page.emulate_media(color_scheme="light")
        page.click(".theme button:has-text('Dark')")
        if theme() != "dark":
            fails.append("Dark did not apply")
        page.reload(); page.wait_for_selector(".theme")
        if theme() != "dark":
            fails.append("Dark lost on reload")
        page.click(".theme button:has-text('System')")
        page.emulate_media(color_scheme="dark")
        page.wait_for_timeout(200)
        if theme() != "dark":
            fails.append("System did not follow the system")

        # 6. Dark mode: nothing stays light by accident.
        page.emulate_media(color_scheme="dark")
        page.set_viewport_size({"width": 1440, "height": 900})
        for p in PAGES:
            go(page, p)
            fails += [f"dark {p}: {x}" for x in page.evaluate(DARK_JS)]

        # 7. Traces: a clicked span opens right under it, in sight, and closes with ×.
        page.emulate_media(color_scheme="light")
        tid = "4bf92f3577b34da6a3ce929d0e0e4736"
        now = time.time_ns() - 10**9
        mk = lambda sid, parent, name, off: {"traceId": tid, "spanId": sid, "name": name, "kind": 3, **({"parentSpanId": parent} if parent else {}),
                                             "startTimeUnixNano": str(now + off * 10**6), "endTimeUnixNano": str(now + (off + 20) * 10**6),
                                             "attributes": [{"key": "retry.attempt", "value": {"intValue": "2"}}]}
        body = {"resourceSpans": [{"resource": {"attributes": [{"key": "service.name", "value": {"stringValue": "acme-api"}}]},
                                   "scopeSpans": [{"spans": [mk("a000000000000001", None, "POST /orders", 0)] + [mk(f"b{i:015x}", "a000000000000001", f"step {i}", i * 30) for i in range(1, 12)]}]}]}
        urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{s.OTLP_HTTP_PORT}/v1/traces", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})).read()
        page.goto(f"{base}/telemetry/traces/{tid}")
        page.wait_for_selector(".spanrow")
        head = page.locator(".spanrow .spanhead").nth(6)
        top = head.bounding_box()["y"]
        head.click()
        box = page.locator(".spanrow.on .spandetail").bounding_box() if page.locator(".spanrow.on .spandetail").count() else None
        if not box or box["y"] < top or box["y"] > top + 80 or box["y"] + 40 > 900:
            fails.append(f"span detail not under the clicked span: {box} (row at {top})")
        page.click(".spanx")
        if page.locator(".spandetail").count():
            fails.append("span detail did not close")

        fails += [f"console: {e}" for e in errors if "integrity" not in e]
        browser.close()
    if fails:
        print(json.dumps(fails, indent=1, ensure_ascii=False))
        sys.exit(f"{len(fails)} problem(s)")
    print("web ok")


if __name__ == "__main__":
    main()
