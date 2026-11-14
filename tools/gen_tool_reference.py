"""Writes docs/reference/tools.md from the engine source: every tool is one MCP tool and one HTTP route.
Run from the repo root: python3 tools/gen_tool_reference.py"""
import re, pathlib

ROOT = pathlib.Path(__file__).resolve().parents[1]
STR = r'"((?:[^"\\]|\\.)*)"'
TOOL = re.compile(r'Tool::new\(\s*' + STR + r'\s*,\s*' + STR)
CALL = re.compile(r'\.(req|param|route)\(\s*' + STR + r'\s*,\s*' + STR + r'\s*\)')
AREAS = [  # (crate, title, file globs)
    ("server", "Environments and timeline", ["crates/server/src/environments.rs", "crates/server/src/timeline.rs", "crates/server/src/main.rs"]),
    ("email", "E-mail", ["crates/email/src/tools.rs"]),
    ("sms", "SMS", ["crates/sms/src/tools.rs"]),
    ("whatsapp", "WhatsApp", ["crates/whatsapp/src/tools.rs"]),
    ("apis", "HTTP APIs (plugins)", ["crates/apis/src/tools.rs", "crates/apis/src/restpki.rs"]),
    ("storage", "Storage (Azure Blob, S3)", ["crates/storage/src/tools.rs"]),
    ("messaging", "Messaging (Service Bus, RabbitMQ)", ["crates/messaging/src/tools.rs"]),
    ("identity", "Identity (OIDC / OAuth2)", ["crates/identity/src/tools.rs"]),
    ("keyvault", "Secrets (Key Vault)", ["crates/keyvault/src/tools.rs"]),
    ("webhook", "Webhook inboxes", ["crates/webhook/src/tools.rs"]),
    ("telemetry", "Telemetry (OpenTelemetry)", ["crates/telemetry/src/tools.rs"]),
]


def unesc(s):
    return s.replace('\\"', '"').replace("\\n", " ").replace("|", "\\|")


def parse(path):
    src = path.read_text()
    hits = list(TOOL.finditer(src))
    out = []
    for i, m in enumerate(hits):
        end = hits[i + 1].start() if i + 1 < len(hits) else len(src)
        body = src[m.end():end]
        req, opt, route = [], [], None
        for c in CALL.finditer(body):
            kind, a, b = c.group(1), c.group(2), c.group(3)
            if kind == "route":
                route = route or f"{a} /api{b}"
            else:
                (req if kind == "req" else opt).append(a)
        out.append((m.group(1), unesc(m.group(2)), req, opt, route))
    return out


# The server merges messaging into storage's tools (engine/crates/server/src/main.rs::shared_messaging_tools):
# messaging_<x> answers through <x> with service = azure-service-bus | rabbitmq, and two more share a name.
SHARED = ["list_rule_options", "list_rules", "test_rule", "create_rule", "update_rule", "set_rule_enabled", "move_rule",
          "delete_rule", "put_rules_yaml", "get_service_schema", "list_service_accounts", "get_account", "create_account",
          "update_account", "rotate_key", "delete_account"]
RENAMED = {"set_next_message_delivery": "set_next_delivery", "wait_for_queue_message": "wait_for_message"}


def merge(areas):
    hidden = {"messaging_" + s for s in SHARED} | set(RENAMED)
    note = {s: " Also for messaging: service = azure-service-bus \\| rabbitmq (accounts are namespaces)." for s in SHARED}
    note.update({v: f" For a queue or topic, pass namespace (and entity)." for v in RENAMED.values()})
    out = []
    for title, tools in areas:
        kept = []
        for name, desc, req, opt, route in tools:
            if name in hidden:
                continue
            kept.append((name, desc + note.get(name, ""), req, opt, route))
        out.append((title, kept))
    return out


lines = ["# Tool reference", "",
         "Every action in Stuntman is one tool. Each tool is an **MCP tool** (same name) and an **HTTP route** under `/api`.",
         "`GET /api/tools` returns this catalog from a running server, with full parameter schemas.", "",
         "_Generated from the engine source by `tools/gen_tool_reference.py`. Do not edit by hand._", ""]
total = 0
index = []
sections = []
parsed = []
for crate, title, files in AREAS:
    tools = []
    for f in files:
        p = ROOT / "engine" / f
        if p.exists():
            tools += parse(p)
    parsed.append((title, tools))
for title, tools in merge(parsed):
    if not tools:
        continue
    total += len(tools)
    anchor = re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-")
    index.append(f"- [{title}](#{anchor}) · {len(tools)} tools")
    sec = [f"## {title}", "", "| Tool | HTTP | Required | Optional | What it does |", "|---|---|---|---|---|"]
    for name, desc, req, opt, route in tools:
        sec.append(f"| `{name}` | {('`' + route + '`') if route else '—'} | {', '.join(req) or '—'} | "
                   f"{', '.join(opt) or '—'} | {desc} |")
    sections += sec + [""]
lines += [f"**{total} tools** in {len(index)} areas.", ""] + index + [""] + sections
(ROOT / "docs" / "reference").mkdir(parents=True, exist_ok=True)
(ROOT / "docs" / "reference" / "tools.md").write_text("\n".join(lines))
print(total, "tools")
