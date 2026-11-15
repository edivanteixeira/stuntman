"""End-to-end check of the productivity catalog plugins: microsoft-graph, google-drive (OAuth with
PKCE, refresh, revoke), calendly (signed webhooks), lemlist (+ MCP), azure-devops (service hooks)
and newrelic (NerdGraph GraphQL). Each is installed from engine/plugins/ as an app would use it.

    python3 tests/e2e_plugins_productivity.py
"""
import base64, hashlib, hmac, http.server, json, os, secrets, threading, time, urllib.parse

from harness import ROOT, Stuntman, free_port, wait

HOOK = free_port()
GOT = []


class Hooks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        GOT.append({"path": self.path, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": self.rfile.read(n).decode()})
        self.send_response(200); self.end_headers()

    def log_message(self, *a):
        pass


def got(prefix):
    return [g for g in GOT if g["path"].startswith(prefix)]


def install(s, name):
    v = s.call("PUT", f"/plugins/{name}", {"yaml": open(os.path.join(ROOT, "plugins", name + ".yaml")).read()})
    assert v["errors"] == [], v
    v = s.call("POST", f"/plugins/{name}/validate", {})
    assert v["ok"], (name, v)


def form(s, path, fields):
    return s.api("POST", path, urllib.parse.urlencode(fields).encode(), {"Content-Type": "application/x-www-form-urlencoded"})


def graph(s):
    install(s, "microsoft-graph")
    B = {"Authorization": "Bearer eyJ0eXAiOi.synthetic"}
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/me")
    assert st == 401 and v["error"]["code"] == "InvalidAuthenticationToken" and "request-id" in v["error"]["innerError"], (st, v)
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/me", headers=B)
    assert st == 200 and v["displayName"] == "Emma Carter" and v["@odata.context"].endswith("#users/$entity"), v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users?$top=5", headers=B)
    assert st == 200 and len(v["value"]) == 3, v
    q = urllib.parse.quote("mail eq 'liam.foster@contoso.example.com'")
    st, v, _ = s.api("GET", f"/microsoft-graph/v1.0/users?$filter={q}", headers=B)
    assert [u["displayName"] for u in v["value"]] == ["Liam Foster"], v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users?$filter=" + urllib.parse.quote("mail eq 'nobody@example.com'"), headers=B)
    assert v["value"] == [], v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/olivia.brooks@contoso.example.com", headers=B)
    assert st == 200 and v["id"] == "48d31887-5fad-4d73-a9f5-3c356e68a038", v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/00000000-0000-0000-0000-000000000000", headers=B)
    assert st == 404 and v["error"]["code"] == "Request_ResourceNotFound", v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/6e7b768e-07e2-4810-8459-485f84f8f204/photo/$value", headers=B)
    assert st == 404 and v["error"]["code"] == "ImageNotFound", v
    q = urllib.parse.quote("resourceProvisioningOptions/Any(x:x eq 'Team')")
    st, v, _ = s.api("GET", f"/microsoft-graph/v1.0/groups?$filter={q}", headers=B)
    team = v["value"][0]["id"]
    assert st == 200 and v["value"][0]["resourceProvisioningOptions"] == ["Team"], v
    st, v, _ = s.api("GET", f"/microsoft-graph/v1.0/teams/{team}", headers=B)
    assert v["displayName"] == "Operations", v
    st, v, _ = s.api("GET", f"/microsoft-graph/v1.0/teams/{team}/channels", headers=B)
    assert [c["displayName"] for c in v["value"]] == ["General", "Incidents"], v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/emma.carter@contoso.example.com/chats", headers=B)
    assert v["@odata.count"] == 2 and v["value"][1]["chatType"] == "group", v
    st, v, _ = s.api("POST", "/microsoft-graph/v1.0/users/emma.carter@contoso.example.com/sendMail",
                     {"message": {"subject": "Hi", "body": {"contentType": "Text", "content": "Hello"}, "toRecipients": [{"emailAddress": {"address": "lily.parker@globex.example.com"}}]}}, B)
    assert st == 202 and v == b"", (st, v)
    st, v, _ = s.api("POST", "/microsoft-graph/v1.0/users/emma.carter@contoso.example.com/sendMail", {"message": {"subject": "Hi", "toRecipients": []}}, B)
    assert st == 400 and v["error"]["code"] == "ErrorInvalidRecipients", v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/6e7b768e-07e2-4810-8459-485f84f8f204/mailFolders/inbox/messages", headers=B)
    assert v["value"][0]["subject"] == "Renewal quote for Globex", v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/6e7b768e-07e2-4810-8459-485f84f8f204/calendarView", headers=B)
    assert st == 400 and v["error"]["code"] == "ErrorInvalidParameter", v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users/6e7b768e-07e2-4810-8459-485f84f8f204/calendarView?startDateTime=2026-11-16T00:00:00Z&endDateTime=2026-11-17T00:00:00Z", headers=B)
    assert v["value"][0]["subject"] == "Weekly operations sync", v
    ev = {"subject": "Globex renewal", "start": {"dateTime": "2026-11-20T15:00:00", "timeZone": "UTC"}, "end": {"dateTime": "2026-11-20T15:30:00", "timeZone": "UTC"}}
    st, v, _ = s.api("POST", "/microsoft-graph/v1.0/users/6e7b768e-07e2-4810-8459-485f84f8f204/events", ev, B)
    assert st == 201 and v["subject"] == "Globex renewal" and v["start"] == ev["start"] and v["id"].startswith("AAMk"), v
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/organization", headers=B)
    assert v["value"][0]["displayName"] == "Contoso", v
    # Change notifications: 201, then the validation handshake to notificationUrl.
    sub = {"changeType": "created", "notificationUrl": f"http://127.0.0.1:{HOOK}/graph", "resource": "/users/6e7b768e-07e2-4810-8459-485f84f8f204/messages",
           "expirationDateTime": "2026-11-18T00:00:00Z", "clientState": "acme-secret"}
    st, v, _ = s.api("POST", "/microsoft-graph/v1.0/subscriptions", sub, B)
    assert st == 201 and v["clientState"] == "acme-secret" and v["latestSupportedTlsVersion"] == "v1_2", v
    h = wait(lambda: got("/graph?validationToken="), "graph validation")[0]
    token = urllib.parse.parse_qs(urllib.parse.urlparse(h["path"]).query)["validationToken"][0]
    assert token.startswith("Validation: Testing client application reachability"), token
    st, v, _ = s.api("POST", "/microsoft-graph/v1.0/subscriptions", {"changeType": "created"}, B)
    assert st == 400 and v["error"]["code"] == "InvalidRequest", v
    # Named answers: throttling and denied.
    s.call("POST", "/apis/microsoft-graph/force", {"answer": "throttled", "count": 1})
    st, v, hd = s.api("GET", "/microsoft-graph/v1.0/users", headers=B)
    assert st == 429 and hd["retry-after"] == "10" and v["error"]["code"] == "TooManyRequests", (st, v, hd)
    s.call("POST", "/apis/microsoft-graph/force", {"answer": "request_denied", "count": 1})
    st, v, _ = s.api("GET", "/microsoft-graph/v1.0/users", headers=B)
    assert st == 403 and v["error"]["code"] == "Authorization_RequestDenied", v
    print("  microsoft-graph ok")


def drive(s):
    install(s, "google-drive")
    verifier = secrets.token_urlsafe(48)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    redirect = "https://app.acme.example.com/oauth/google"
    qs = urllib.parse.urlencode({"client_id": "acme-drive.apps.googleusercontent.com", "redirect_uri": redirect, "response_type": "code", "scope": "https://www.googleapis.com/auth/drive.file",
                                 "state": "st-58213", "access_type": "offline", "code_challenge": challenge, "code_challenge_method": "S256"})
    st, page, _ = s.api("GET", "/google-drive/o/oauth2/v2/auth?" + qs, headers={"User-Agent": "Mozilla/5.0"})
    assert st == 200 and b"Sign in with Google" in page and b"Emma Carter" in page, (st, page[:300])
    st, bad, _ = s.api("GET", "/google-drive/o/oauth2/v2/auth?client_id=x&redirect_uri=" + urllib.parse.quote(redirect), headers={"User-Agent": "Mozilla/5.0"})
    assert st == 400 and b"code_challenge" in bad, st
    pending = s.call("GET", "/apis/steps?service=google-drive")
    assert pending["steps"] if isinstance(pending, dict) else pending, pending
    a = s.call("POST", "/apis/steps/authorize", {"service": "google-drive", "person": "Emma Carter"})
    loc = urllib.parse.urlparse(a["redirect"])
    q = urllib.parse.parse_qs(loc.query)
    assert a["redirect"].startswith(redirect) and q["state"] == ["st-58213"], a
    code = q["code"][0]
    st, v, _ = form(s, "/google-drive/token", {"grant_type": "authorization_code", "code": code, "code_verifier": "wrong" + verifier, "redirect_uri": redirect, "client_id": "acme-drive"})
    assert st == 400 and v["error"] == "invalid_grant", (st, v)
    st, v, _ = form(s, "/google-drive/token", {"grant_type": "authorization_code", "code": code, "code_verifier": verifier, "redirect_uri": redirect, "client_id": "acme-drive"})
    assert st == 200 and v["token_type"] == "Bearer" and v["access_token"], (st, v)
    st, v2, _ = form(s, "/google-drive/token", {"grant_type": "authorization_code", "code": code, "code_verifier": verifier, "redirect_uri": redirect})
    assert st == 400 and v2["error"] == "invalid_grant", v2
    access = v["access_token"]
    refresh = "1//0gAcmeEmmaCarterRefreshTokenSynthetic"
    st, r, _ = form(s, "/google-drive/token", {"grant_type": "refresh_token", "refresh_token": refresh, "client_id": "acme-drive", "client_secret": "x"})
    assert st == 200 and r["access_token"].startswith("ya29.") and r["expires_in"] == 3599, (st, r)
    B = {"Authorization": "Bearer " + access}
    st, v, _ = s.api("GET", "/google-drive/drive/v3/about?fields=user", headers=B)
    assert st == 200 and v["user"]["emailAddress"] == "emma.carter@acme.example.com" and "storageQuota" not in v, v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/about", headers=B)
    assert st == 400, v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files?pageSize=2", headers={"Authorization": "Bearer " + r["access_token"]})
    assert st == 200 and v["nextPageToken"] == "page2" and len(v["files"]) == 2, v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files?pageToken=page2", headers=B)
    assert "nextPageToken" not in v and v["files"][0]["name"] == "Budget 2027", v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files?q=" + urllib.parse.quote("mimeType = 'application/vnd.google-apps.folder' and trashed = false"), headers=B)
    assert [f["name"] for f in v["files"]] == ["Contracts"], v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files/1Zx8mQ4vKp2sT7aB3cD9eF0gH1iJ2kL3m", headers=B)
    assert set(v) == {"kind", "id", "name", "mimeType"}, v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files/1Zx8mQ4vKp2sT7aB3cD9eF0gH1iJ2kL3m?fields=id,name,size,parents", headers=B)
    assert v["size"] == "48211", v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files/nope", headers=B)
    assert st == 404 and v["error"]["errors"][0]["reason"] == "notFound", v
    st, v, _ = s.api("POST", "/google-drive/drive/v3/files", {"name": "Invoices", "mimeType": "application/vnd.google-apps.folder"}, B)
    assert st == 200 and v["name"] == "Invoices", v
    st, v2, _ = s.api("GET", f"/google-drive/drive/v3/files/{v['id']}", headers=B)
    assert v2["name"] == "Invoices" and v2["mimeType"] == "application/vnd.google-apps.folder", v2
    body = (b"--b1\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{\"name\": \"notes.txt\"}\r\n--b1\r\nContent-Type: text/plain\r\n\r\nhello drive\r\n--b1--\r\n")
    st, v, _ = s.api("POST", "/google-drive/upload/drive/v3/files?uploadType=multipart", body, dict(B, **{"Content-Type": "multipart/related; boundary=b1"}))
    assert st == 200 and v["kind"] == "drive#file" and v["id"], (st, v)
    st, v, _ = s.api("POST", "/google-drive/upload/drive/v3/files?uploadType=multipart", b"hello", dict(B, **{"Content-Type": "text/plain"}))
    assert st == 400 and "multipart" in v["error"]["message"], v
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files")
    assert st == 401 and v["error"]["status"] == "UNAUTHENTICATED", v
    # Revoke: the refresh token then answers invalid_grant; a revoked access token gets 401.
    st, _, _ = form(s, "/google-drive/revoke", {"token": refresh})
    assert st == 200, st
    st, v, _ = form(s, "/google-drive/token", {"grant_type": "refresh_token", "refresh_token": refresh})
    assert st == 400 and v == {"error": "invalid_grant", "error_description": "Token has been expired or revoked."}, v
    st, _, _ = s.api("POST", "/google-drive/revoke?token=" + access)
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files", headers=B)
    assert st == 401 and v["error"]["errors"][0]["reason"] == "authError", v
    s.call("POST", "/apis/google-drive/force", {"answer": "rate_limit_exceeded", "count": 1})
    st, v, _ = s.api("GET", "/google-drive/drive/v3/files", headers={"Authorization": "Bearer ya29.other"})
    assert st == 403 and v["error"]["errors"][0]["reason"] == "rateLimitExceeded", v
    print("  google-drive ok")


def calendly(s):
    install(s, "calendly")
    B = {"Authorization": "Bearer pat-acme-synthetic"}
    st, v, _ = s.api("GET", "/calendly/users/me")
    assert st == 401 and v["title"] == "Unauthenticated", v
    st, me, _ = s.api("GET", "/calendly/users/me", headers=B)
    org, user = me["resource"]["current_organization"], me["resource"]["uri"]
    assert st == 200 and me["resource"]["resource_type"] == "User", me
    st, v, _ = s.api("GET", "/calendly/organization_memberships?organization=" + urllib.parse.quote(org), headers=B)
    assert len(v["collection"]) == 2 and v["pagination"]["count"] == 2, v
    st, v, _ = s.api("GET", "/calendly/event_types", headers=B)
    assert st == 400 and v["title"] == "Invalid Argument", v
    st, et, _ = s.api("GET", "/calendly/event_types?user=" + urllib.parse.quote(user), headers=B)
    assert et["collection"][0]["name"] == "30 Minute Demo", et
    st, v, _ = s.api("GET", "/calendly/scheduled_events?user=" + urllib.parse.quote(user), headers=B)
    assert [e["status"] for e in v["collection"]] == ["active", "canceled"], v
    st, v, _ = s.api("GET", "/calendly/scheduled_events/EV0GLOBEX0000001/invitees", headers=B)
    assert v["collection"][0]["email"] == "lily.parker@globex.example.com", v
    st, v, _ = s.api("POST", "/calendly/webhook_subscriptions", {"url": f"http://127.0.0.1:{HOOK}/calendly", "events": ["invitee.created", "invitee.canceled"], "organization": org, "scope": "organization"}, B)
    assert st == 201 and v["resource"]["state"] == "active", (st, v)
    hook_uri = v["resource"]["uri"]
    st, v, _ = s.api("POST", "/calendly/webhook_subscriptions", {"url": f"http://127.0.0.1:{HOOK}/calendly", "events": ["invitee.created"], "organization": org, "scope": "organization"}, B)
    assert st == 409, (st, v)
    st, v, _ = s.api("GET", f"/calendly/webhook_subscriptions?organization={urllib.parse.quote(org)}&scope=organization", headers=B)
    assert [h["uri"] for h in v["collection"]] == [hook_uri], v
    # Scheduling API: book, invitee.created is signed like Calendly.
    st, v, _ = s.api("POST", "/calendly/invitees", {"event_type": et["collection"][0]["uri"], "start_time": "2026-11-20T15:00:00Z",
                                                    "invitee": {"name": "Owen Hughes", "email": "owen.hughes@globex.example.com", "timezone": "America/New_York"}}, B)
    assert st == 201 and v["resource"]["email"] == "owen.hughes@globex.example.com", (st, v)
    h = wait(lambda: got("/calendly"), "invitee.created")[0]
    sig = dict(x.split("=", 1) for x in h["headers"]["calendly-webhook-signature"].split(","))
    want = hmac.new(b"calendly-acme-signing-key", f"{sig['t']}.{h['body']}".encode(), hashlib.sha256).hexdigest()
    assert sig["v1"] == want and abs(int(sig["t"]) - time.time()) < 60, (sig, want)
    p = json.loads(h["body"])
    assert p["event"] == "invitee.created" and p["payload"]["email"] == "owen.hughes@globex.example.com", p
    # Cancel: 201, then invitee.canceled; twice: 403.
    st, v, _ = s.api("POST", "/calendly/scheduled_events/EV0GLOBEX0000001/cancellation", {"reason": "Customer asked to move it"}, B)
    assert st == 201 and v["resource"]["canceler_type"] == "host", (st, v)
    h = wait(lambda: [g for g in got("/calendly") if '"invitee.canceled"' in g["body"]], "invitee.canceled")[0]
    p = json.loads(h["body"])
    assert p["payload"]["cancellation"]["reason"] == "Customer asked to move it" and p["payload"]["email"] == "lily.parker@globex.example.com", p
    st, v, _ = s.api("POST", "/calendly/scheduled_events/EV0GLOBEX0000001/cancellation", {}, B)
    assert st == 403, (st, v)
    st, v, _ = s.api("POST", "/calendly/invitee_no_shows", {"invitee": "https://api.calendly.com/scheduled_events/EV0GLOBEX0000001/invitees/INV0LILYPARKER01"}, B)
    assert st == 201 and v["resource"]["invitee"].endswith("INV0LILYPARKER01"), v
    # Deleted subscription: no more deliveries.
    st, _, _ = s.api("DELETE", "/calendly/webhook_subscriptions/" + hook_uri.rsplit("/", 1)[1], headers=B)
    assert st == 204, st
    st, v, _ = s.api("GET", f"/calendly/webhook_subscriptions?organization={urllib.parse.quote(org)}&scope=organization", headers=B)
    assert v["collection"] == [], v
    n = len(got("/calendly"))
    s.api("POST", "/calendly/invitees", {"event_type": et["collection"][0]["uri"], "start_time": "2026-11-21T15:00:00Z", "invitee": {"email": "zoe.fleming@globex.example.com"}}, B)
    time.sleep(0.8)
    assert len(got("/calendly")) == n, "deleted subscription must not deliver"
    s.call("POST", "/apis/calendly/force", {"answer": "rate_limited", "count": 1})
    st, v, hd = s.api("GET", "/calendly/users/me", headers=B)
    assert st == 429 and hd["x-ratelimit-reset"] == "30", (st, hd)
    print("  calendly ok")


def mcp(s, msg, sid=None):
    h = {"Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-06-18"}
    if sid:
        h["Mcp-Session-Id"] = sid
    return s.api("POST", "/lemlist/mcp", msg, h)


def lemlist(s):
    install(s, "lemlist")
    K = {"Authorization": "Basic " + base64.b64encode(b":lemlist-key-synthetic").decode()}
    st, v, _ = s.api("GET", "/lemlist/api/team")
    assert st == 401, st
    st, v, _ = s.api("GET", "/lemlist/api/team", headers=K)
    assert v["name"] == "Acme Growth", v
    st, v, _ = s.api("GET", "/lemlist/api/campaigns?access_token=lemlist-key-synthetic")
    assert st == 200 and v[0]["_id"] == "cam_Q4OutboundSaaS", v
    st, v, _ = s.api("GET", "/lemlist/api/campaigns?version=v2", headers=K)
    assert v["pagination"]["totalRecords"] == 2, v
    st, v, _ = s.api("POST", "/lemlist/api/campaigns/cam_Q4OutboundSaaS/leads/owen.hughes@globex.example.com", {"firstName": "Owen", "lastName": "Hughes", "companyName": "Globex"}, K)
    assert st == 200 and v["campaignName"] == "Q4 outbound - SaaS" and v["_id"].startswith("lea_"), v
    st, v, _ = s.api("POST", "/lemlist/api/campaigns/cam_GlobexFollowUp/leads/owen.hughes@globex.example.com?deduplicate=true", {}, K)
    assert st == 409, (st, v)
    st, v, _ = s.api("POST", "/lemlist/api/campaigns/cam_nope/leads/a@example.com", {}, K)
    assert st == 404 and v == b"Campaign not found", (st, v)
    st, v, _ = s.api("GET", "/lemlist/api/leads/owen.hughes@globex.example.com", headers=K)
    assert v[0]["firstName"] == "Owen", v
    st, v, _ = s.api("GET", "/lemlist/api/leads/lily.parker@globex.example.com", headers=K)
    assert v[0]["campaignId"] == "cam_Q4OutboundSaaS", v
    st, v, _ = s.api("POST", "/lemlist/api/leads/pause/lily.parker@globex.example.com", {}, K)
    assert v[0]["isPaused"] is True, v
    st, v, _ = s.api("POST", "/lemlist/api/hooks", {"targetUrl": "https://app.acme.example.com/lemlist", "type": "emailsReplied"}, K)
    assert v["_id"].startswith("hoo_"), v
    st, v2, _ = s.api("DELETE", f"/lemlist/api/hooks/{v['_id']}", headers=K)
    assert st == 200 and v2["_id"] == v["_id"], v2
    st, v, _ = s.api("GET", "/lemlist/api/hooks", headers=K)
    assert v[0]["targetUrl"].startswith("https://crm.acme"), v
    s.call("POST", "/apis/lemlist/force", {"answer": "rate_limited", "count": 1})
    st, v, hd = s.api("GET", "/lemlist/api/team", headers=K)
    assert st == 429 and hd["retry-after"] == "2", st
    # MCP.
    st, v, h = mcp(s, {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "acme-agent", "version": "1"}}})
    sid = h.get("mcp-session-id")
    assert st == 200 and v["result"]["serverInfo"]["name"] == "lemlist", v
    mcp(s, {"jsonrpc": "2.0", "method": "notifications/initialized"}, sid)
    st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}, sid)
    assert [t["name"] for t in v["result"]["tools"]] == ["get_campaigns", "add_leads_to_campaign", "get_team_info"], v
    st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "get_campaigns", "arguments": {}}}, sid)
    assert json.loads(v["result"]["content"][0]["text"])[1]["name"] == "Globex follow-up", v
    st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "add_leads_to_campaign", "arguments": {"campaignId": "cam_Q4OutboundSaaS", "leads": [{"email": "caleb.ross@globex.example.com"}]}}}, sid)
    r = json.loads(v["result"]["content"][0]["text"])
    assert v["result"]["isError"] is False and r["added"] == 1, v
    st, v, _ = mcp(s, {"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "add_leads_to_campaign", "arguments": {"campaignId": "cam_x", "leads": []}}}, sid)
    assert v["result"]["isError"] is True, v
    print("  lemlist ok")


def devops(s):
    install(s, "azure-devops")
    P = {"Authorization": "Basic " + base64.b64encode(b":pat-synthetic").decode()}
    st, v, hd = s.api("GET", "/azure-devops/acme/_apis/projects?api-version=7.1")
    assert st == 203 and hd["content-type"].startswith("text/html"), (st, hd)
    st, v, _ = s.api("GET", "/azure-devops/acme/_apis/projects", headers=P)
    assert st == 400 and v["typeKey"] == "VssVersionNotSpecifiedException" and v["eventId"] == 3000 and "\"GET\"" in v["message"], v
    st, v, _ = s.api("GET", "/azure-devops/acme/_apis/projects?api-version=7.1", headers=P)
    assert v["count"] == 2 and v["value"][0]["name"] == "Storefront", v
    st, v, _ = s.api("GET", "/azure-devops/acme/Nope/_apis/git/repositories/storefront-web/pullrequests?api-version=7.1", headers=P)
    assert st == 404 and v["typeKey"] == "ProjectDoesNotExistException" and v["$id"] == "1" and v["innerException"] is None, v
    st, v, _ = s.api("GET", "/azure-devops/acme/Storefront/_apis/git/repositories/nope/pullrequests?api-version=7.1", headers=P)
    assert st == 404 and v["typeKey"] == "GitRepositoryNotFoundException", v
    st, v, _ = s.api("GET", "/azure-devops/acme/Storefront/_apis/git/repositories/storefront-web/pullrequests?api-version=7.1&searchCriteria.status=active", headers=P)
    assert v["value"][0]["pullRequestId"] == 2201, v
    sub = {"publisherId": "tfs", "eventType": "git.pullrequest.created", "resourceVersion": "1.0", "consumerId": "webHooks", "consumerActionId": "httpRequest",
           "publisherInputs": {"projectId": "6ce954b1-ce1f-45d1-b94d-e6bf2464ba2c", "repository": ""},
           "consumerInputs": {"url": f"http://127.0.0.1:{HOOK}/devops", "httpHeaders": "X-Acme-Secret:s3cr3t"}}
    st, v, _ = s.api("POST", "/azure-devops/acme/_apis/hooks/subscriptions?api-version=7.1", sub, P)
    assert st == 200 and v["id"] == "9b1c2d3e-0001-4000-8000-000000000001" and v["status"] == "enabled", (st, v)
    pr = {"sourceRefName": "refs/heads/feature/invoices", "targetRefName": "refs/heads/main", "title": "Invoices export"}
    st, v, _ = s.api("POST", "/azure-devops/acme/Storefront/_apis/git/repositories/storefront-web/pullrequests?api-version=7.1", pr, P)
    assert st == 201 and v["status"] == "active" and v["title"] == "Invoices export", (st, v)
    h = wait(lambda: got("/devops"), "service hook")[0]
    p = json.loads(h["body"])
    assert p["eventType"] == "git.pullrequest.created" and p["resource"]["pullRequestId"] == v["pullRequestId"] and p["subscriptionId"] == "9b1c2d3e-0001-4000-8000-000000000001", p
    st, _, _ = s.api("DELETE", "/azure-devops/acme/_apis/hooks/subscriptions/9b1c2d3e-0001-4000-8000-000000000001?api-version=7.1", headers=P)
    assert st == 204, st
    s.api("POST", "/azure-devops/acme/Storefront/_apis/git/repositories/storefront-web/pullrequests?api-version=7.1", pr, P)
    time.sleep(0.8)
    assert len(got("/devops")) == 1, "deleted subscription must not deliver"
    print("  azure-devops ok")


def newrelic(s):
    install(s, "newrelic")
    K = {"API-Key": "NRAK-SYNTHETIC0000000000000000"}
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": "{ actor { user { name email } } }"})
    assert st == 401 and v["errors"][0]["message"], v
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": "{ actor { user { name email } accounts { id name } } }"}, K)
    assert v["data"]["actor"]["user"]["email"] == "emma.carter@acme.example.com" and len(v["data"]["actor"]["accounts"]) == 2, v
    q = "query($accountId: Int!, $nrql: Nrql!) { actor { account(id: $accountId) { nrql(query: $nrql) { results } } } }"
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": q, "variables": {"accountId": 1234567, "nrql": "SELECT count(*) FROM Transaction FACET appName SINCE 1 hour ago"}}, K)
    assert v["data"]["actor"]["account"]["nrql"]["results"][0]["facet"] == "storefront-web", v
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": '{ actor { account(id: 1234567) { nrql(query: "SELECT count(*) FROM Transaction SINCE 1 hour ago") { results } } } }'}, K)
    assert v["data"]["actor"]["account"]["nrql"]["results"] == [{"count": 15873}], v
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": q, "variables": {"accountId": 1234567, "nrql": "SELECT count(*) FROM Transaction TIMESERIES"}}, K)
    assert len(v["data"]["actor"]["account"]["nrql"]["results"]) == 3, v
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": q, "variables": {"accountId": 999, "nrql": "SELECT 1 FROM Transaction"}}, K)
    assert v["data"]["actor"]["account"] is None and v["errors"][0]["path"] == ["actor", "account"], v
    st, v, _ = s.api("POST", "/newrelic/graphql", {"query": "{ docs { x } }"}, K)
    assert "errors" in v, v
    print("  newrelic ok")


def main():
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hooks)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    with Stuntman("stuntman-productivity-") as s:
        graph(s)
        drive(s)
        calendly(s)
        lemlist(s)
        devops(s)
        newrelic(s)
    srv.shutdown()
    print("productivity plugins e2e: ok")


if __name__ == "__main__":
    main()
