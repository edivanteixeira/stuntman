"""Builds the Stuntman site into site/dist: plain HTML, one shared stylesheet, no JavaScript.
Run: python3 site/build.py   (the GitHub Pages workflow runs the same command)."""
import html, pathlib, shutil

HERE = pathlib.Path(__file__).resolve().parent
OUT = HERE / "dist"
REPO = "https://github.com/edivanteixeira/stuntman"
DOCS = REPO + "/blob/main/docs"
PAGES = [("index", "Home"), ("agents", "Agents"), ("providers", "Providers"), ("failures", "Failures"),
         ("timeline", "Timeline"), ("plugins", "Plugins"), ("environments", "Environments"), ("start", "Get started")]
TITLES = {"index": "Stuntman — the stunt double for every provider your app calls", "agents": "Agents", "providers": "Providers",
          "failures": "Failures on cue", "timeline": "Timeline and proof", "plugins": "Plugins", "environments": "Environments",
          "start": "Get started"}
DESC = {"index": "Stuntman stands in for SMS, WhatsApp, email, storage, queues, identity, secrets and any HTTP API, "
                 "so you and your coding agents can test the hard cases end to end, over MCP or HTTP.",
        "agents": "Claude Code, Codex, Gemini CLI, OpenCode, Cursor and GitHub Copilot test their own work against Stuntman over MCP or HTTP.",
        "providers": "Every kind of provider Stuntman stands in for, what it keeps and what you can provoke.",
        "failures": "Make the next call fail, exactly as asked: rules, forced answers and every provocation, line by line.",
        "timeline": "See what your app sent, in the provider's words, and find the calls that never came through.",
        "plugins": "A provider is a plugin; a plugin is one YAML file. The format, expressions and how to install one.",
        "environments": "One Stuntman, an environment per branch: clone, pause, expire and route requests.",
        "start": "Paste one prompt into your coding agent: it starts Stuntman, configures your project and proves nothing leaks."}
e = html.escape


def rel(page, target):
    up = "" if page == "index" else "../"
    return up + ("" if target == "index" else target + "/")


def asset(page, path):
    return ("" if page == "index" else "../") + "assets/" + path


def nav(page):
    links = "".join(
        f'<a href="{rel(page, s)}"{" aria-current=\"page\"" if s == page else ""}>{label}</a>'
        for s, label in PAGES[1:-1])
    return (f'<header class="nav"><div class="wrap nav__in">'
            f'<a class="wordmark" href="{rel(page, "index")}">Stuntman</a>'
            f'<nav class="nav__links" aria-label="Sections">{links}</nav>'
            f'<a class="nav__gh" href="{REPO}">GitHub</a>'
            f'<a class="btn" href="{rel(page, "start")}">Get started</a></div></header>')


def footer(page):
    links = "".join(f'<a href="{rel(page, s)}">{label}</a>' for s, label in PAGES[1:])
    return (f'<footer class="footer"><div class="wrap footer__in"><div><a class="wordmark" href="{rel(page, "index")}">Stuntman</a>'
            f'<p>The provider\'s stunt double. Open source (Apache-2.0), built in the open.</p></div>'
            f'<nav aria-label="Footer">{links}<a href="{REPO}">GitHub</a><a href="{DOCS}/reference/tools.md">Tool reference</a></nav></div></footer>')


def next_link(page):
    order = [p for p, _ in PAGES[1:]]
    nxt = order[(order.index(page) + 1) % len(order)] if page in order else "agents"
    return (f'<div class="wrap next"><a href="{rel(page, nxt)}"><small>Next</small>'
            f'<span class="display" style="font-size:var(--text-xl)">{TITLES[nxt]} →</span></a></div>')


def doc(page, body):
    title = TITLES[page] if page == "index" else f"{TITLES[page]} · Stuntman"
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{e(title)}</title>
<meta name="description" content="{e(DESC[page])}">
<meta property="og:title" content="{e(title)}">
<meta property="og:description" content="{e(DESC[page])}">
<meta property="og:image" content="{asset(page, 'og.png')}">
<meta name="theme-color" content="#FAFAF8">
<link rel="icon" href="{asset(page, 'favicon.svg')}" type="image/svg+xml">
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Archivo:wdth,wght@62..125,400..800&family=JetBrains+Mono:wght@400;500&display=swap">
<link rel="stylesheet" href="{asset(page, 'site.css')}">
</head>
<body>
<a class="skip" href="#main">Skip to content</a>
{nav(page)}
<main id="main">
{body}
</main>
{footer(page)}
{COPY_JS}
</body>
</html>
"""


# ------------------------------------------------------------------ building blocks
def art(page, name, w, h, alt, mobile=None):
    m = f'<source media="(max-width: 700px)" srcset="{asset(page, "art/" + name + "-m.webp")}">' if mobile else ""
    return (f'<figure class="art"><picture>{m}<img src="{asset(page, "art/" + name + ".webp")}" width="{w}" height="{h}" '
            f'alt="{e(alt)}" loading="lazy" decoding="async"></picture></figure>')


def shot(page, name, alt, pins=()):
    p = "".join(f'<span class="num" style="--x:{x:.1f}%;--y:{y:.1f}%" aria-hidden="true">{n}</span>' for n, x, y in pins)
    return (f'<figure class="shot"><img src="{asset(page, "screens/" + name + ".webp")}" width="1920" height="1200" '
            f'alt="{e(alt)}" loading="lazy" decoding="async">{p}</figure>')


def pin(n, x, y):  # 1440 x 900 screen coordinates → percent
    return (n, x / 1440 * 100, y / 900 * 100)


def head(level, title, text=None, cls=""):
    t = f'<p class="body">{text}</p>' if text else ""
    return f'<div class="section__head {cls}"><h{level} class="display">{title}</h{level}>{t}</div>'


def code(text, dark=False):
    return f'<pre class="code{" dark" if dark else ""}"><code>{e(text)}</code></pre>'


def session(rows):
    out = []
    for kind, a, b in rows:
        if kind == "say":
            out.append(f'<div class="say"><span class="who">{a}</span><p>{e(b)}</p></div>')
        elif kind == "tool":
            out.append(f'<div class="tool"><b>{a}</b><span>{e(b)}</span></div>')
        else:
            out.append(f'<div class="res{" bad" if a == "bad" else ""}">→ {e(b)}</div>')
    return f'<div class="session" role="group" aria-label="Agent session">{"".join(out)}</div>'


PROMPT = ("Set up Stuntman for this project: read https://edivanteixeira.github.io/stuntman/agent-setup.md "
          "and follow it step by step.")


def prompt_box(page, label="Paste this into your coding agent"):
    return (f'<div class="prompt"><p class="prompt__label">{label}</p>'
            f'<div class="prompt__row"><p class="prompt__text" id="prompt-{page}">{e(PROMPT)}</p>'
            f'<button class="btn prompt__copy" type="button" data-copy="prompt-{page}">Copy</button></div>'
            f'<p class="prompt__hint">It asks which language you prefer, starts Stuntman, finds every provider your project '
            f'calls, shows you each change before making it and ends with every link ready. '
            f'<span aria-live="polite" class="prompt__status"></span></p></div>')


COPY_JS = """<script>
document.querySelectorAll('[data-copy]').forEach(function (b) {
  b.addEventListener('click', function () {
    var t = document.getElementById(b.dataset.copy).textContent, s = b.parentNode.parentNode.querySelector('.prompt__status');
    function done(ok) { b.textContent = ok ? 'Copied' : 'Copy'; if (s) s.textContent = ok ? 'Copied to the clipboard.' : 'Select the text and copy it.';
      setTimeout(function () { b.textContent = 'Copy'; if (s) s.textContent = ''; }, 2500); }
    if (navigator.clipboard) navigator.clipboard.writeText(t).then(function () { done(true); }, function () { done(false); }); else done(false);
  });
});
</script>"""


AGENTS = [("Claude Code", "claude"), ("Codex", None), ("Gemini CLI", "googlegemini"), ("OpenCode", "opencode"),
          ("Cursor", "cursor"), ("GitHub Copilot", "githubcopilot")]


def agents_strip(page):
    items = "".join(
        (f'<span class="agent"><img src="{asset(page, "icons/" + s + ".svg")}" alt="" width="22" height="22">{n}</span>' if s
         else f'<span class="agent agent--text">{n}</span>') for n, s in AGENTS)
    return f'<div class="agents"><div class="wrap agents__in"><p>Works with</p>{items}</div></div>'


def hero(page, title, lead, actions=True):
    crumb = "" if page == "index" else f'<p class="crumb"><a href="{rel(page, "index")}">stuntman</a> / {PAGES[[p for p, _ in PAGES].index(page)][1].lower()}</p>'
    if page == "index":
        act = (prompt_box(page) + '<p class="hero__alt">Prefer to do it yourself? '
               '<code>docker run -p 8025:8025 edivanteixeira/stuntman</code> · '
               f'<a href="{rel(page, "start")}">step by step</a></p>')
    else:
        act = (f'<div class="hero__actions"><a class="btn" href="{rel(page, "start")}">Get started</a>'
               '<span class="endpoint"><span><b>MCP</b>/mcp</span><span><b>HTTP</b>/api</span></span></div>') if actions else ""
    return f'<section class="wrap hero">{crumb}<h1 class="display">{title}</h1><p class="lead">{lead}</p>{act}</section>'


def faq(items):
    return '<dl class="faq">' + "".join(f"<div><dt>{q}</dt><dd>{a}</dd></div>" for q, a in items) + "</dl>"


def line(items, timed=False):
    lis = "".join(f'<li class="{c}">{f"<time>{t}</time>" if t else ""}<b>{h}</b><span>{s}</span></li>' for c, t, h, s in items)
    return f'<ol class="line{" timed" if timed else ""}" style="--n:{len(items)}">{lis}</ol>'


def table(head_cells, rows):
    th = "".join(f"<th scope=\"col\">{h}</th>" for h in head_cells)
    trs = "".join("<tr>" + "".join((f'<th scope="row">{c}</th>' if i == 0 else f"<td>{c}</td>") for i, c in enumerate(r)) + "</tr>"
                  for r in rows)
    return f'<table class="table"><thead><tr>{th}</tr></thead><tbody>{trs}</tbody></table>'


def chip(color, text):
    return f'<span class="chip" style="--c:var(--line-{color})" aria-hidden="true"></span>{text}'


# ------------------------------------------------------------------ pages
def home():
    p = "index"
    rules_crop = (f'<figure class="shot" style="aspect-ratio:940/374"><img src="{asset(p, "screens/rules.webp")}" width="1920" height="1200" '
                  f'alt="The rule: turned on by claude-code via MCP; what the app gets: 503, 503, then 201" loading="lazy" '
                  f'style="width:153.2%;max-width:none;margin-left:-52.1%;margin-top:-10.2%"></figure>')
    return doc(p, f"""
{hero(p, "Watch your coding agent prove its own fix.",
      "Claude Code, Codex, Gemini CLI, OpenCode or any agent that speaks MCP or plain HTTP. Stuntman stands in for the "
      "providers your app calls, so the agent can set up staging, break those providers on purpose, read what your app "
      "did and leave the proof where you can see it.")}
{agents_strip(p)}
<section class="wrap section"><div class="split">
  {head(2, "It tests the whole app. No one clicks.", "The agent runs the loop on its own, as many times as the change needs. Each lap leaves calls on the timeline under the agent's name.")}
  {art(p, "loop", 2095, 1204, "A ring of seven stations: read the code, set up Stuntman, change the code, run the real flow, provoke failures, read what the app sent, check the result", True)}
</div></section>
<section class="wrap section">
  {head(2, "It reads your code and sets Stuntman up.", "The agent finds every provider your app calls, installs the matching plugins and points the base URLs at Stuntman. Then it checks that nothing still goes out to a real provider.")}
  {art(p, "sets-up", 2656, 1284, "Each source file that calls a provider becomes a line; every line passes through the agent and ends at Stuntman with a plugin installed. .env base URLs now point at Stuntman; real_provider_calls finds none left.", True)}
  <p class="note" style="margin-top:var(--space-md)">Illustrative: file, provider and plugin names are examples. The tool names are real.</p>
</section>
<section class="wrap section">
  {head(2, "Before and after.", "Before: staging talks to real providers. After: every base URL points at Stuntman, each provider has its plugin and the agent proves no call still leaks out.")}
  {art(p, "env-before-after", 2656, 1116, "A staging .env before, pointing at real providers, and after, pointing at Stuntman; then list_plugins, install_plugin, set_provider, edit .env, real_provider_calls: none", True)}
</section>
<section class="wrap section">
  {head(2, "One afternoon in staging.", "In the real UI, with synthetic data. Real tool names and argument shapes; some arguments are left out.")}
  <article class="chapter"><div class="chapter__text"><h3 class="display"><span class="num">1</span>You ask for the hard case.</h3>
    <p class="body">Not the happy path: the one where the storage provider fails twice in a row.</p>
    {session([("say", "you", "Make contract uploads fail twice with a 503, then tell me whether acme-api recovers."),
              ("tool", "create_rule", '{"when": {"op": "put", "path": "contracts/**"}, "then": {"respond": "503"}, "how_often": {"first": 2}}'),
              ("res", "ok", "rule on, recorded as claude-code via MCP")])}</div>
    {shot(p, "rules", "The storage rule “Contract uploads fail twice,” turned on by claude-code via MCP", [pin(1, 470, 137)])}</article>
  <article class="chapter"><div class="chapter__text"><h3 class="display"><span class="num">2</span>It reads what your app actually did.</h3>
    <p class="body">Not what the code says it does. Every call your app made is on the timeline, in the provider's words.</p>
    {session([("tool", "wait_for_object", '{"container": "documents", "prefix": "contracts/nda-acme"}'),
              ("res", "bad", "408: no new object"),
              ("say", "agent", "Nothing was stored. The timeline shows two 503s, then acme-api gave up. The retry limit is the bug.")])}</div>
    {shot(p, "timeline", "The timeline: the upload of nda-acme-signed.pdf failed with 503, provoked by the rule; attempt 1 and 2 both 503", [pin(2, 930, 130)])}</article>
  <article class="chapter"><div class="chapter__text"><h3 class="display"><span class="num">3</span>It fixes the code and runs the flow again.</h3>
    <p class="body">The rule is still on, so the second run meets the same two failures. This time the app has to get through them.</p></div>
    {session([("say", "agent", "Raised the upload attempts in BlobUploader from 2 to 3, with backoff. Running the signing flow again."),
              ("tool", "wait_for_object", '{"container": "documents", "prefix": "contracts/nda-acme"}'),
              ("res", "ok", "nda-acme-signed.pdf stored, on the third attempt"),
              ("tool", "set_rule_enabled", '{"name": "Contract uploads fail twice", "enabled": false}'),
              ("res", "ok", "rule off"),
              ("say", "agent", "The fix holds: the third attempt stored the file. I turned the rule off. The calls are on the timeline under my name.")])}</article>
  <article class="chapter"><div class="chapter__text"><h3 class="display"><span class="num">4</span>You check the evidence, not the agent's word.</h3>
    <p class="body">The rule shows who turned it on, which requests it matched and what the app gets on each try. Nothing rests on the agent's summary.</p></div>
    {rules_crop}</article>
</section>
<section class="wrap section"><div class="split">
  <div class="stack">{head(2, "One prompt. The whole app tested.", "You ask once. The agent sets up, provokes, reads and checks every provider path, then hands back a report you can verify in the UI.")}
  <blockquote class="session"><div class="say"><span class="who">you</span><p>Test sign-up end to end against Stuntman: SMS down, a WhatsApp reply, an email bounce, login with MFA. Fix what breaks.</p></div></blockquote></div>
  {art(p, "run-log", 1416, 1576, "The agent's run: set up, SMS down, WhatsApp reply, email bounce, login with MFA, avatar upload failed on retry limit 2, fix, avatar upload stored on attempt 3, check: no calls to real providers", True)}
</div></section>
<section class="wrap section">
  {head(2, "Any agent. Same switches.", "Anything that speaks MCP, or plain HTTP, drives Stuntman the way a person does in the UI. Every MCP tool is also an HTTP route; <code>GET /api/tools</code> lists them all.")}
  {art(p, "any-agent", 2418, 1176, "Claude Code, Codex, Gemini CLI, OpenCode, Cursor and GitHub Copilot, each a line through MCP or HTTP into Stuntman", True)}
  <div class="doors" style="margin-top:var(--space-2xl)">
    <div><h3>Over MCP</h3>{code('create_rule {"service": "azure-blob", "name": "Contract uploads fail twice",\n  "when": {"op": "put", "path": "contracts/**"},\n  "then": {"respond": "503"}, "how_often": {"first": 2}}')}</div>
    <div><h3>Over HTTP</h3>{code('POST /api/services/azure-blob/rules\n{"name": "Contract uploads fail twice",\n "when": {"op": "put", "path": "contracts/**"},\n "then": {"respond": "503"}, "how_often": {"first": 2}}')}</div>
  </div>
</section>
<section class="wrap section"><div class="split">
  {head(2, "No plugin for your provider yet? Ask.", "A provider is one YAML file: routes, state, answers, callbacks and the sentence each call leaves. The tools that read a plugin can also write it, so the agent drafts it and you review it in the UI, with the YAML beside it.")}
  {shot(p, "route-editor", "The route editor: receive, keep, decide, respond, send, with the plugin YAML beside it")}
</div></section>
<section class="wrap section">
  {faq([("What does my app point at?", "Its provider base URLs. Stuntman answers on them with the provider's own paths, bodies and status codes."),
        ("Can each pull request get its own?", "Yes. A pipeline clones <code>stg-a</code> into <code>pr-5462</code> for its agent, and the copy expires by itself."),
        ("Is it safe to expose?", "Not yet. There is no sign-in. Keep it on a private network or behind a proxy that authenticates.")])}
</section>
""")


def agents():
    p = "agents"
    clients = [("Claude Code", "claude mcp add --transport http stuntman \\\n  http://localhost:8025/mcp"),
               ("Codex", "codex mcp add stuntman --url http://localhost:8025/mcp"),
               ("Gemini CLI · settings.json", '{ "mcpServers": { "stuntman":\n  { "httpUrl": "http://localhost:8025/mcp" } } }'),
               ("OpenCode · opencode.json", '{ "mcp": { "stuntman": { "type": "remote",\n  "url": "http://localhost:8025/mcp" } } }'),
               ("Cursor · .cursor/mcp.json", '{ "mcpServers": { "stuntman":\n  { "url": "http://localhost:8025/mcp" } } }'),
               ("GitHub Copilot · .vscode/mcp.json", '{ "servers": { "stuntman": { "type": "http",\n  "url": "http://localhost:8025/mcp" } } }')]
    cl = "".join(f"<div><h3>{n}</h3>{code(c)}</div>" for n, c in clients)
    groups = [("Watch", "list_sms · list_whatsapp_messages · list_api_calls · list_traces · wait_for_object · wait_for_api_call"),
              ("Provoke", "create_rule · set_rule_enabled · force_answer · force_sms_failure · force_whatsapp_failure · expire_tokens · send_bad_webhook · provoke_sign_in"),
              ("Play the people", "reply_as_user · reply_sms_as_user · simulate_recipient"),
              ("Set up", "list_plugins · install_plugin · import_spec · start_recording · make_plugin · set_provider · create_environment"),
              ("Check", "real_provider_calls · validate_plugin · list_forced · would_fail")]
    return doc(p, f"""
{hero(p, "Built for agents that test their own work.",
      "Claude Code, Codex, Gemini CLI, OpenCode, Cursor and GitHub Copilot connect over MCP. Anything else uses plain HTTP. "
      "Both reach the same 306 tools, and the timeline records who acted.")}
{agents_strip(p)}
<section class="wrap section">
  {head(2, "The loop it runs on its own.")}
  {line([("", "", "Read the code", "finds the providers your app calls"), ("", "", "Set up Stuntman", "installs plugins, points base URLs at Stuntman"),
         ("", "", "Change the code", "the fix or the feature you asked for"), ("", "", "Run the real flow", "your app, end to end"),
         ("pv", "", "Provoke failures", "the cases staging never shows"), ("", "", "Read what the app sent", "calls, retries, callbacks"),
         ("end", "", "Check the result", "against the timeline, not a guess")])}
</section>
<section class="wrap section">
  {head(2, "Connect it in one line.", "MCP runs over streamable HTTP at <code>/mcp</code>; each environment also has <code>/e/&lt;env&gt;/mcp</code>. Client formats change: if one does not load, check its docs for “remote” or “streamable HTTP” servers.")}
  <div class="trio">{cl}</div>
  <div class="warn" style="margin-top:var(--space-xl)"><strong>There is no API key yet.</strong>Anyone who reaches the port can use every tool. Per-agent keys are planned; until then, keep Stuntman private.</div>
</section>
<section class="wrap section"><div class="split">
  {head(2, "Or plain HTTP.", "Every tool is an HTTP route under <code>/api</code>, with the same arguments. Pass <code>by</code> to name the actor on the timeline; MCP clients are named automatically.")}
  {code('curl -s localhost:8025/api/tools | jq ".[].name"\n\ncurl -s -X POST localhost:8025/api/whatsapp/force \\\n  -H "content-type: application/json" \\\n  -d \'{"answer": "500", "count": 1, "by": "codex"}\'', True)}
</div></section>
<section class="wrap section">
  {head(2, "It sets Stuntman up, too.", "Ask it to read the repository, find every provider, install the plugins, point the staging URLs at Stuntman and prove nothing leaks.")}
  {art(p, "sets-up", 2656, 1284, "Source files become provider lines through the agent into Stuntman", True)}
</section>
<section class="wrap section">
  {head(2, "What an agent can do.", "A sample of the real tool names, grouped by intent. The full list is in the <a href='" + DOCS + "/reference/tools.md'>tool reference</a>.")}
  {table(["Intent", "Tools"], [(g, f'<code>{t}</code>') for g, t in groups])}
</section>
<section class="wrap section"><div class="split">
  {head(2, "Tips for reliable runs.")}
  <ul class="stack body" style="max-width:none">
    <li><b>Wait, do not sleep.</b> <code>wait_for_object</code>, <code>wait_for_api_call</code> and <code>wait_for_delivery</code> return the moment the thing happens, or fail with a clear timeout.</li>
    <li><b>Fail a set number of times, not forever.</b> <code>count</code>, <code>first</code> and <code>for</code> turn a failure off by themselves, so a crashed run leaves no trap behind.</li>
    <li><b>One environment per pull request.</b> See <a href="{rel(p, 'environments')}">Environments</a>.</li>
    <li><b>Ask for evidence.</b> A good report points at the timeline, where every call carries the actor's name.</li>
  </ul>
</div></section>
{next_link(p)}""")


def providers():
    p = "providers"
    L = [("blue", "HTTP APIs", "REST · OAuth2 client credentials · Basic · session cookies · mTLS · browser steps",
          "Routes, state per route and data tables, written in YAML.", "Named answers, latency, 5xx, reset, timeout, expired tokens, rate limits.", "route-editor",
          "Point your app at <code>http://localhost:8028/&lt;service&gt;</code>."),
         ("blue", "SOAP services", "SOAP 1.1 and 1.2 · made from the WSDL · WS&#8209;Security UsernameToken",
          "Operations from the WSDL with the same steps as routes; bodies checked against its XSD; the WSDL served back.",
          "Any fault the WSDL declares, the provider's validation fault, refused credentials, slow or failed calls.", "soap",
          f"Create it with <code>import_wsdl</code> or <b>+ New service</b>, then point your app at <code>http://localhost:8028/&lt;service&gt;&lt;endpoint&gt;</code>. <a href='{DOCS}/soap.md'>SOAP guide</a>."),
         ("blue", "OpenAPI specs", "OpenAPI 3.0 and 3.1 · Swagger 2.0 · JSON or YAML",
          "Routes from the paths; answers from the examples, or built from the schemas and echoing what the app sent; the spec served back.",
          "Any response the spec declares, the provider's 400 or 422 for a body that breaks the schema, expired tokens, slow or failed calls.", "openapi",
          f"Create it with <code>import_spec</code> or <b>+ New service</b>, then point your app at <code>http://localhost:8028/&lt;service&gt;</code>. <a href='{DOCS}/openapi.md'>OpenAPI guide</a>."),
         ("blue", "Recording", "Your provider's sandbox · any HTTP API with no spec",
          "Each call goes to the sandbox and back, with secrets, cards, CPFs and e-mails masked; the review groups calls into routes, answers and rules.",
          "Once the plugin is written: every answer the sandbox gave, plus anything a plugin can provoke. The sandbox is no longer called.", "recording",
          f"Start it with <code>start_recording</code> or <b>+ New service</b>, stop, review, <code>make_plugin</code>. <a href='{DOCS}/recording.md'>Recording guide</a>."),
         ("red", "SMS", "Zenvia API v2", "Messages, delivery steps per recipient, opt-outs.", "500, 400, reset, empty reply, timeout; the person texts back, STOP and START.", None,
          "Point your app at <code>http://localhost:8025/zenvia/v2/channels/{0}/messages</code>."),
         ("green", "WhatsApp", "Meta WhatsApp Cloud API (Graph)", "Conversations, templates, the 24-hour window and the recipients.",
          "500, 400, connection failures, expired tokens, replies, wrongly signed or replayed webhooks.", "whatsapp", "Graph base URL <code>http://localhost:8025/whatsapp</code>."),
         ("amber", "Email", "SMTP with STARTTLS · SendGrid v3 · signed Event Webhook", "Every message with its HTML, attachments and extracted codes and links.",
          "Hold and decide: deliver, defer, bounce, drop; open, click, spam report, unsubscribe.", "email", "SMTP <code>localhost:2525</code> · SendGrid <code>http://localhost:8025/sendgrid</code>."),
         ("brown", "Storage", "Azure Blob (SharedKey, SAS) · Amazon S3 (SigV4)", "Accounts, containers, objects, uploads, presigned links and their events.",
          "503 on the first N attempts, 403/404, slow or truncated reads, bad links, lost or duplicated events.", "objects", "Blob <code>:10000/&lt;account&gt;</code> · S3 <code>:9000</code>."),
         ("teal", "Messaging", "Azure Service Bus · RabbitMQ (AMQP)", "Queues, topics, subscriptions, every message with its deliveries, consumers and locks.",
          "Lost locks, duplicates, held, dropped, dead-lettered or out-of-order messages; a busy or refusing namespace.", "queues", "AMQP <code>:5673</code> (TLS <code>:5671</code>)."),
         ("green", "Identity", "OIDC / OAuth2: Microsoft Entra ID, Google, Okta, generic OIDC", "Tenants, apps, synthetic users and their claims; a sign-in page you can drive.",
          "MFA, consent, user cancels, wrong tenant, expired tokens, bad signatures, IdP down.", "login-page", "Use the authority shown in the provider's settings in the UI."),
         ("graphite", "Secrets", "Azure Key Vault · managed identity", "Versions, keys that really sign and wrap, certificates, read counts that prove caching.",
          "403, 404, 429, disabled, slow, expired or untrusted certificates, wrong signatures.", "secrets", "<code>https://&lt;vault&gt;.vault.&lt;base domain&gt;:8444</code>; trust the Test CA."),
         ("orange", "Webhook inboxes", "Endpoints your app sends its own webhooks to", "Every delivery with its signature check; inboxes that expire by themselves.",
          "200, 500, timeout, hang, delay, fail twice then succeed.", "inbox-behaviour", "<code>http://localhost:8027/in/&lt;inbox&gt;</code>."),
         ("amber", "Telemetry", "OpenTelemetry: OTLP logs, traces, metrics", "Spans joined to the provider calls that caused them.",
          "A collector that refuses, rejects, throttles, slows or drops half; hosts still called directly.", "traces", "<code>OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318</code>.")]
    secs = []
    for i, (c, name, flav, keeps, prov, sc, point) in enumerate(L):
        txt = (f'<div class="stack"><p>{chip(c, "")}</p><h2 class="display" id="{name.lower().replace(" ", "-")}">{name}</h2>'
               f'<p class="mono" style="color:var(--text-{c if c not in ("graphite",) else "red"});font-size:var(--text-sm)">{flav}</p>'
               f'<p class="body"><b style="color:var(--color-ink)">It keeps.</b> {keeps}</p>'
               f'<p class="body"><b style="color:var(--color-ink)">You can provoke.</b> {prov}</p><p class="note">{point}</p></div>')
        img = shot(p, sc, f"The {name} screen") if sc else ""
        secs.append(f'<section class="wrap section"><div class="split">{txt}{img}</div></section>')
    return doc(p, hero(p, "Every provider your app calls. None of them real.",
                       "Each line is one kind of provider, such as SMS, storage or identity; a flavor or a plugin is one vendor's version of it. Stuntman "
                       "answers like the provider, keeps its state and sends its callbacks back.", actions=False)
               + "".join(secs) + next_link(p))


def failures():
    p = "failures"
    rows = [("HTTP APIs", "force_answer · expire_tokens", "any named answer or SOAP fault; reset, empty, timeout, 500, 503, HTML; invalid tokens"),
            ("SMS", "force_sms_failure · reply_sms_as_user", "500, 400, reset, empty, timeout; the person texts back"),
            ("WhatsApp", "force_whatsapp_failure · send_bad_webhook", "500, 400, reset, empty, timeout; wrong, missing or old signature, replay"),
            ("Email", "decide_held · simulate_recipient", "deliver, defer, bounce, drop; open, click, spam report, unsubscribe"),
            ("Storage", "create_rule · set_next_read · set_next_use", "any status, first N or a percentage; slow, truncated, bad MD5; bad links"),
            ("Messaging", "set_next_delivery (namespace) · set_namespace_reachability", "lose lock, duplicate, hold, drop, dead-letter, out of order; refuse, busy"),
            ("Identity", "provoke_sign_in · provoke_idp", "MFA, consent, user cancels, wrong tenant, expired, IdP down"),
            ("Secrets", "provoke_secret · provoke_key · provoke_certificate", "403, 404, 429, disabled, slow, expired, untrusted"),
            ("Webhook inboxes", "set_inbox", "fail N times then succeed, delay, hang, redirect"),
            ("Telemetry", "provoke_collector", "401, 403, 429, 503, refused, slow, drop half")]
    colors = ["blue", "red", "green", "amber", "brown", "teal", "green", "graphite", "orange", "amber"]

    def dots(pattern):
        return '<span class="dots">' + "".join(f'<i class="{"on" if x else ""}"></i>' for x in pattern) + "</span>"
    return doc(p, f"""
{hero(p, "Make the next call fail. Exactly as asked.",
      "A rule says which requests, what to answer and how often. You, a test suite or an agent can turn it on, and the "
      "timeline marks every answer it changed.", actions=False)}
<section class="wrap section">
  {head(2, "How often is part of the rule.")}
  <ol class="often">
    <li><b>The first 2, then normal</b>{dots([1, 1, 0, 0, 0, 0, 0, 0])}</li>
    <li><b>Every time</b>{dots([1] * 8)}</li>
    <li><b>Randomly, 25% of the requests</b>{dots([0, 1, 0, 0, 0, 1, 0, 0])}</li>
  </ol>
  <p class="legend"><span><i class="on"></i>answered as the rule says</span><span><i></i>answered normally</span></p>
</section>
<section class="wrap section">
  {head(2, "What you can provoke, line by line.", "Arguments for every tool are in the <a href='" + DOCS + "/reference/tools.md'>tool reference</a>.")}
  {table(["Line", "Tools", "Answers"], [(chip(colors[i], r[0]), f"<code>{r[1]}</code>", r[2]) for i, r in enumerate(rows)])}
</section>
<section class="wrap section"><div class="split">
  <div class="stack">{head(2, "A rule is a file, too.", "Edit it in the UI and try it against the last 50 requests before saving. It is saved in <code>storage/rules/&lt;service&gt;.yaml</code>, and an agent creates the same rule with one tool call.")}
  {code('create_rule {\n  "service": "azure-blob",\n  "name": "Contract uploads fail twice",\n  "when": {"op": "put", "container": "documents",\n           "path": "contracts/**"},\n  "then": {"respond": "503"},\n  "how_often": {"first": 2}\n}')}</div>
  {shot(p, "rules", "The rule editor: when, then, how often and what the app will see", [pin(1, 470, 137)])}
</div></section>
<section class="wrap section"><div class="split">
  <div class="stack">{head(2, "Callbacks your app must verify.", "Stuntman signs callbacks the way each provider does: ECDSA for SendGrid, the app secret for Meta, HMAC for your own inboxes. Ask for a badly signed or replayed one and check that your app rejects it.")}
  {code('send_bad_webhook {"kind": "wrong_signature"}\n→ acme-api answered 401')}</div>
  {shot(p, "inbox-deliveries", "Webhook deliveries with their signature checks and attempts")}
</div></section>
{next_link(p)}""")


def timeline():
    p = "timeline"
    notes = [("A sentence per call", "Acme KYC verified ID 4417-••••, not a JSON body."), ("Who did it", "the app, you, an agent over MCP, a test over HTTP."),
             ("What was provoked", "marked on the row, with the rule that did it."), ("Where to go next", "the object, the rule, the trace, raw HTTP.")]
    nl = "".join(f'<li><span class="num">{i + 1}</span><div><b>{a}</b><p>{b}</p></div></li>' for i, (a, b) in enumerate(notes))
    return doc(p, f"""
{hero(p, "See what your app sent, in the provider's words.",
      "Every call becomes a sentence: who sent it, what came back, how long it took. Raw HTTP is one click away when you need it.", actions=False)}
<section class="wrap">
  {shot(p, "timeline", "The timeline with numbered marks", [pin(1, 875, 188), pin(2, 352, 478), pin(3, 930, 130), pin(4, 1092, 290)])}
  <ol class="notes">{nl}</ol>
</section>
<section class="wrap section">
  {head(2, "Calls that never came through.", "Requests with no route get their own list, with a “Create a route from this request” action. Telemetry shows the hosts your app still calls directly, so a forgotten base URL cannot hide.")}
  <div class="leak" role="img" aria-label="acme-api calls Stuntman, and one forgotten call still goes to api.twilio.com">
    <span class="box">acme-api</span><span class="ok"></span><span class="hub">Stuntman</span>
    <span class="bad"></span><span class="real">api.twilio.com</span>
  </div>
  <p class="mono" style="color:var(--text-red);font-size:var(--text-sm);margin-top:var(--space-lg)">real_provider_calls → 1 host outside Stuntman: api.twilio.com</p>
</section>
<section class="wrap section"><div class="split">
  {head(2, "The trace ID makes the round trip.", "Stuntman takes your app's trace ID on the way in and sends it back on callbacks. Spans, logs and metrics live in their own area and open from a row, so the timeline stays readable.")}
  {shot(p, "traces", "Traces joined to the provider calls Stuntman answered")}
</div></section>
<section class="wrap section">
  {head(2, "For agents and tests.", "Read the same timeline over MCP or HTTP, and wait instead of sleeping.")}
  {table(["Need", "Tools"], [("Wait for it", "<code>wait_for_object · wait_for_api_call · wait_for_delivery · wait_for_link</code>"),
                             ("Read calls", "<code>list_api_calls · list_sms · list_whatsapp_messages · list_events · list_sign_ins</code>"),
                             ("Who forced what", "<code>list_forced</code>"), ("What escaped", "<code>list_unmatched · real_provider_calls</code>")])}
</section>
{next_link(p)}""")


def plugins():
    p = "plugins"
    yaml = ("service:\n  name: recaptcha\n  real_host: www.google.com\nroutes:\n  - route: POST /recaptcha/api/siteverify\n"
            "    receive:\n      body: [secret, response]\n    keep:\n      state: tokens\n      key: body.response\n"
            "    decide:\n      - when: body.response in state.tokens\n        answer: duplicate\n      - answer: human\n"
            "answers:\n  - name: duplicate\n    sentence: \"Token used twice: timeout-or-duplicate\"")
    return doc(p, f"""
{hero(p, "A provider is a plugin. A plugin is a file.",
      "Routes, the state they keep, the answers they give, the callbacks they send and the sentence each call leaves behind. "
      "Written once by someone who knows the provider; installed by everyone else.", actions=False)}
<section class="wrap section">
  {head(2, "Anatomy of a plugin.")}
  <div class="anatomy">{code(yaml)}
  <dl><div><dt>service</dt><dd>where it pretends to live, and the real host it stands in for</dd></div>
      <div><dt>routes</dt><dd>one per kind of request; each reads receive → keep → decide → respond → send</dd></div>
      <div><dt>keep</dt><dd>the state the provider remembers between calls</dd></div>
      <div><dt>decide</dt><dd>plain expressions; the first match wins; no code</dd></div>
      <div><dt>answers</dt><dd>the provider's bodies, and the sentence the timeline shows</dd></div></dl></div>
</section>
<section class="wrap section"><div class="split">
  {head(2, "Edit in the UI. Keep the YAML.", "The UI edits the same file. Every change is validated before it is saved, and the YAML beside the editor shows the exact lines that changed.")}
  {shot(p, "route-editor", "The route editor with the YAML beside it")}
</div></section>
<section class="wrap section">
  {head(2, "Install it three ways.", "<code>validate_plugin</code> returns schema and expression errors with line numbers before anything is saved.")}
  <div class="trio"><div><h3>From the catalog</h3>{code("list_plugins\ninstall_plugin recaptcha")}</div>
    <div><h3>From a folder or a .zip</h3>{code("import_plugin ./plugins/acme-kyc\ninstall_import")}</div>
    <div><h3>From a git URL</h3>{code("import_plugin https://example.com/acme/plugins.git\n  path=kyc ref=v1.2.0")}</div></div>
</section>
<section class="wrap section"><div class="split">
  {head(2, "Safe by default.", "A plugin cannot reach the network or the disk unless it declares it. Expressions are a small, side-effect-free language.")}
  <p class="body">Full format, expressions and contribution guide: <a href="{DOCS}/plugins.md">docs/plugins.md</a>. The catalog lives in <a href="{REPO}/tree/main/engine/plugins">engine/plugins</a>.</p>
</div></section>
{next_link(p)}""")


def environments():
    p = "environments"
    return doc(p, f"""
{hero(p, "One Stuntman. An environment per branch.",
      "Each environment has its own plugins, state, rules, inboxes, base URLs and MCP endpoint. Pause it and the state is kept; "
      "clone it for a pull request and let it expire.", actions=False)}
<section class="wrap">{shot(p, "environments", "Environments: STG-A, STG-B (paused), STG-C and pr-5451, cloned by a pipeline, expiring in 22 h")}</section>
<section class="wrap section">
  {head(2, "The life of pr-5462.")}
  {line([("", "09:00", "Cloned from stg-a", "services, rules, inboxes and seeds; no traffic history"),
         ("", "09:01", "The pipeline points the app", "provider URLs at *.pr-5462.stuntman.local"),
         ("", "09:05", "An agent tests the branch", "over /e/pr-5462/mcp"),
         ("", "12:40", "Paused for review", "calls answer 503; the state is kept"),
         ("later", "next day", "Expires by itself", "deleted, state and all")], timed=True)}
  <div style="margin-top:var(--space-xl);max-width:46rem">{code('curl -X POST localhost:8025/api/environments \\\n  -H "content-type: application/json" \\\n  -d \'{"name": "pr-5462", "from": "stg-a", "expires": "24h"}\'')}</div>
</section>
<section class="wrap section">
  {head(2, "A request finds its environment.", "A request that matches none of these goes to the default environment.")}
  <div class="trio"><div><h3>By host name</h3>{code("otp.pr-5462.stuntman.local")}</div>
    <div><h3>By path prefix</h3>{code("localhost:8025/e/pr-5462/mcp")}</div>
    <div><h3>By gRPC metadata</h3>{code("x-stuntman-env: pr-5462")}</div></div>
</section>
<section class="wrap section"><div class="split">
  {head(2, "Clone, start empty or start from files.", "Pick what comes along: services and their files, rules, inboxes, secrets and, optionally, the state. Set how long it lives and where its callbacks go.")}
  {shot(p, "new-environment", "New environment: a copy, empty or from files")}
</div></section>
{next_link(p)}""")


def start():
    p = "start"
    steps = [("Run it", "One container serves the web UI, the HTTP API and MCP. Data lives in one volume.",
              "docker run -d --name stuntman -p 127.0.0.1:8025:8025 \\\n  -p 127.0.0.1:2525:2525 -p 127.0.0.1:8028:8028 \\\n  -v stuntman-data:/data edivanteixeira/stuntman\n\n# web UI, /api and /mcp on http://localhost:8025", True),
             ("Point your app at it", "Change base URLs only. Your app keeps its code and every other setting.",
              "WhatsApp (Graph)   http://localhost:8025/whatsapp\nSendGrid           http://localhost:8025/sendgrid\nSMTP               localhost:2525\nHTTP API plugins   http://localhost:8028/<service>\nAzure Blob         http://127.0.0.1:10000/<account>\nAmazon S3          http://127.0.0.1:9000\nOpenTelemetry      http://localhost:4318", False),
             ("Connect your agent", "Claude Code, Codex, Gemini CLI, OpenCode, Cursor or GitHub Copilot over MCP; anything else over HTTP.",
              "claude mcp add --transport http stuntman \\\n  http://localhost:8025/mcp", False),
             ("Ask for the hard case", "Not the happy path. The agent sets up, provokes, reads and checks, then reports back.",
              "“Make contract uploads fail twice with a 503,\n  then tell me whether acme-api recovers.”", False)]
    arts = "".join(f'<article class="chapter"><div class="chapter__text"><h2 class="display" style="font-size:var(--text-xl);display:flex;gap:var(--space-sm);align-items:baseline">'
                   f'<span class="num">{i + 1}</span>{h}</h2><p class="body">{t}</p></div>{code(c, dark)}</article>'
                   for i, (h, t, c, dark) in enumerate(steps))
    return doc(p, f"""
{hero(p, "Up in one prompt.", "Paste one line into your coding agent. It starts Stuntman, finds every provider your project calls, points your local configuration at it, proves nothing leaks and hands you every link. It shows you each change before making it.", actions=False)}
<section class="wrap">{prompt_box(p, "The fastest way: paste this into your coding agent")}
<p class="note" style="margin-top:var(--space-md)">The agent follows <a href="../agent-setup.md">agent-setup.md</a>, an open guide you can read first.</p></section>
<section class="wrap section">{head(2, "Or do it yourself.")}{arts}</section>
<section class="wrap section">
  <div class="warn"><strong>No sign-in yet.</strong>Anyone who reaches the port can read every message and change every rule. Keep Stuntman on a private network or a VPN, or behind a proxy that authenticates.</div>
  <p class="body" style="margin-top:var(--space-xl)">More ports, Docker Compose and every setting: <a href="{DOCS}/getting-started.md">getting started</a> and <a href="{DOCS}/configuration.md">configuration</a>.</p>
</section>
{next_link(p)}""")


BUILD = {"index": home, "agents": agents, "providers": providers, "failures": failures, "timeline": timeline,
         "plugins": plugins, "environments": environments, "start": start}

if __name__ == "__main__":
    if OUT.exists():
        shutil.rmtree(OUT)
    shutil.copytree(HERE / "assets", OUT / "assets")
    for f in (HERE / "static").iterdir():   # agent-setup.md, llms.txt: served from the site root
        shutil.copy(f, OUT / f.name)
    for name, fn in BUILD.items():
        target = OUT / ("index.html" if name == "index" else f"{name}/index.html")
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(fn())
    (OUT / ".nojekyll").write_text("")
    print("built", len(BUILD), "pages into", OUT)
