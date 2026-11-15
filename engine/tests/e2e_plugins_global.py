"""End-to-end check of the Global catalog plugins (twilio, paypal, adyen, shopify, slack, hubspot, github,
sentry, auth0): validated, installed, driven by the providers' official Python SDKs where they exist (raw HTTP
otherwise), an error and a forced failure each, and every webhook received here with its signature checked
the way the provider documents (the SDK's own verifier when it has one).

    cargo build -p stuntman && <python with the SDKs> tests/e2e_plugins_global.py [plugin…]

SDKs: twilio Adyen slack_sdk hubspot-api-client PyGithub sentry-sdk auth0-python paypal-server-sdk pyjwt.
"""
import base64, hashlib, hmac, http.server, json, os, sys, threading, time, urllib.parse

from harness import Stuntman, free_port, wait

HOOK = free_port()
GOT = []
PLUGINS = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "plugins")
NAMES = ["twilio", "paypal", "adyen", "shopify", "slack", "hubspot", "github", "sentry", "auth0"]
SECRETS = {
    "twilio-auth-token": "acme_twilio_auth_token_0001",
    "paypal-webhook-key": "acme-paypal-transmission-key",
    "shopify-webhook-secret": "acme_shopify_app_secret",
    "slack-signing-secret": "8f742231b10e8888abcd99yyyzzz85a5",
    "hubspot-client-secret": "acme-hubspot-client-secret",
    "github-webhook-secret": "acme-github-hook-secret",
}
ADYEN_HMAC = "44782DEF547AAA06C910C43932B1EB0C71FC68D9D0C057550C48EC2ACF6BA056"


class Hooks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n).decode()
        GOT.append({"path": self.path, "method": self.command, "headers": {k.lower(): v for k, v in self.headers.items()}, "body": raw})
        out, ct = b"", "text/plain"
        if self.path.startswith("/slack"):
            b = json.loads(raw)
            if b.get("type") == "url_verification":
                out, ct = json.dumps({"challenge": b["challenge"]}).encode(), "application/json"
        if self.path.startswith("/adyen"):
            out = b"[accepted]"
        self.send_response(200); self.send_header("Content-Type", ct); self.send_header("Content-Length", str(len(out))); self.end_headers(); self.wfile.write(out)

    def log_message(self, *a):
        pass


def hooks(prefix, pred=lambda g: True):
    return [g for g in GOT if g["path"].startswith(prefix) and pred(g)]


def hook_url(name, rest=""):
    return f"http://127.0.0.1:{HOOK}/{name}{rest}"


# ---------------------------------------------------------------------------------------------- twilio
def t_twilio(s):
    from twilio.rest import Client
    from twilio.base.exceptions import TwilioRestException
    from twilio.request_validator import RequestValidator
    sid, token = "AC0acme0000000000000000000000000001", SECRETS["twilio-auth-token"]
    c = Client(sid, token)
    c.api.base_url = c.verify.base_url = f"http://127.0.0.1:{s.APIS_PORT}/twilio"
    cb = hook_url("twilio", "/status?order=58213")
    m = c.messages.create(to="+12025550123", from_="+15005550006", body="Your Acme order shipped", status_callback=cb)
    assert m.sid.startswith("SM") and m.status == "queued" and m.direction == "outbound-api" and m.to == "+12025550123", m
    # Both callbacks, signed: the SDK's validator checks X-Twilio-Signature over URL + sorted params.
    got = wait(lambda: (lambda x: x if len(x) >= 2 else None)(hooks("/twilio/status")), "twilio callbacks", 10)
    v = RequestValidator(token)
    for g in got:
        params = dict(urllib.parse.parse_qsl(g["body"]))
        assert g["headers"]["content-type"].startswith("application/x-www-form-urlencoded"), g["headers"]
        assert v.validate(cb, params, g["headers"]["x-twilio-signature"]), g
        assert params["MessageSid"] == m.sid and params["To"] == "+12025550123", params
    assert [dict(urllib.parse.parse_qsl(g["body"]))["MessageStatus"] for g in got] == ["sent", "delivered"], got
    assert not RequestValidator("another-token").validate(cb, dict(urllib.parse.parse_qsl(got[0]["body"])), got[0]["headers"]["x-twilio-signature"])
    assert c.messages(m.sid).fetch().status == "delivered"
    # Errors: Twilio's codes.
    for to, code in [("+15005550001", 21211), ("12345", 21211), ("+12025550142", 21610), ("+15005550009", 21614)]:
        try:
            c.messages.create(to=to, from_="+15005550006", body="hi")
            raise AssertionError("no error for " + to)
        except TwilioRestException as e:
            assert e.code == code and e.status == 400, (to, e.code, e.msg)
    st, v2, _ = s.api("POST", f"/twilio/2010-04-01/Accounts/{sid}/Messages.json", b"To=%2B12025550123&Body=x", {"Content-Type": "application/x-www-form-urlencoded"})
    assert st == 401 and v2["code"] == 20003, v2
    # Forced: too many requests.
    s.call("POST", "/apis/twilio/force", {"answer": "too_many_requests", "count": 1})
    try:
        c.messages.create(to="+12025550123", from_="+15005550006", body="hi")
        raise AssertionError("not forced")
    except TwilioRestException as e:
        assert e.status == 429 and e.code == 20429, e
    # Verify v2.
    svc = "VA0acme0000000000000000000000000001"
    ver = c.verify.v2.services(svc).verifications.create(to="+12025550123", channel="sms")
    assert ver.status == "pending" and ver.channel == "sms" and ver.sid.startswith("VE"), ver
    chk = c.verify.v2.services(svc).verification_checks.create(to="+12025550123", code="000000")
    assert chk.status == "pending" and chk.valid is False, chk
    chk = c.verify.v2.services(svc).verification_checks.create(to="+12025550123", code="123456")
    assert chk.status == "approved" and chk.valid is True, chk
    try:
        c.verify.v2.services(svc).verification_checks.create(to="+12025550123", code="123456")
        raise AssertionError("approved twice")
    except TwilioRestException as e:
        assert e.status == 404 and e.code == 20404, e


# ---------------------------------------------------------------------------------------------- slack
def t_slack(s):
    from slack_sdk import WebClient
    from slack_sdk.errors import SlackApiError
    from slack_sdk.signature import SignatureVerifier
    web = WebClient(token="xoxb-acme-0001-bot", base_url=f"http://127.0.0.1:{s.APIS_PORT}/slack/api/")
    a = web.auth_test()
    assert a["ok"] and a["team_id"] == "T0ACME0001" and a["user"] == "acmebot", a.data
    r = web.chat_postMessage(channel="C0200OPS", text="Deploy 2026.11.15 finished", blocks=[{"type": "section", "text": {"type": "mrkdwn", "text": "*Deploy* done"}}])
    assert r["ok"] and r["channel"] == "C0200OPS" and r["message"]["text"] == "Deploy 2026.11.15 finished" and r["message"]["blocks"][0]["type"] == "section", r.data
    r = web.chat_postMessage(channel="#general", text="Hello")
    assert r["channel"] == "C0100GEN", r.data
    chans = web.conversations_list()
    assert [c["name"] for c in chans["channels"]][:2] == ["general", "ops-alerts"], chans.data
    assert web.users_info(user="U0100EMMA")["user"]["profile"]["email"] == "emma.carter@acme.example.com"
    for call, err in [(lambda: web.chat_postMessage(channel="C0NOPE", text="x"), "channel_not_found"), (lambda: web.chat_postMessage(channel="C0300SAL", text="x"), "not_in_channel"),
                      (lambda: web.chat_postMessage(channel="C0400OLD", text="x"), "is_archived"), (lambda: web.users_info(user="U0NOBODY"), "user_not_found"),
                      (lambda: WebClient(token="bad", base_url=web.base_url).auth_test(), "invalid_auth")]:
        try:
            call()
            raise AssertionError("no error " + err)
        except SlackApiError as e:
            assert e.response["ok"] is False and e.response["error"] == err, e.response.data
    # Forced: ratelimited is a 429 with Retry-After.
    s.call("POST", "/apis/slack/force", {"answer": "ratelimited", "count": 1})
    try:
        web.chat_postMessage(channel="C0200OPS", text="x")
        raise AssertionError("not forced")
    except SlackApiError as e:
        assert e.response.status_code == 429 and e.response["error"] == "ratelimited" and e.response.headers["retry-after"] == "30", e.response.headers
    # Incoming webhooks.
    from slack_sdk.webhook import WebhookClient
    wh = WebhookClient(f"http://127.0.0.1:{s.APIS_PORT}/slack/services/T0ACME0001/B0ACMEHOOK/XXXXXXXXacmeOpsHook000001")
    r = wh.send(text="Disk 91% on db-01")
    assert r.status_code == 200 and r.body == "ok", (r.status_code, r.body)
    r = WebhookClient(f"http://127.0.0.1:{s.APIS_PORT}/slack/services/T0ACME0001/B0ACMEHOOK/revoked").send(text="x")
    assert r.status_code == 404 and r.body == "no_service", (r.status_code, r.body)
    r = wh.send(blocks=None, text=None, attachments=None, response_type="in_channel")
    assert r.status_code == 400 and r.body == "no_text", (r.status_code, r.body)
    # Events API: signed requests to the app; url_verification first.
    s.call("PUT", "/plugins/slack/data/settings", {"rows": [{"events_url": hook_url("slack", "/events"), "verification_token": "Jhj5dZrVaK7ZwHHjRyZWjbDl"}]})
    verifier = SignatureVerifier(SECRETS["slack-signing-secret"])
    st, v, _ = s.api("POST", "/slack/simulate/events", {"type": "url_verification"})
    assert st == 200 and v["challenge"], v
    g = wait(lambda: hooks("/slack/events", lambda g: json.loads(g["body"])["type"] == "url_verification"), "url_verification")[0]
    assert json.loads(g["body"])["challenge"] == v["challenge"], g
    assert verifier.is_valid(g["body"], g["headers"]["x-slack-request-timestamp"], g["headers"]["x-slack-signature"]), g["headers"]
    s.api("POST", "/slack/simulate/events", {"type": "app_mention", "text": "<@U0300BOT> deploy status?", "channel": "C0200OPS"})
    g = wait(lambda: hooks("/slack/events", lambda g: json.loads(g["body"]).get("event", {}).get("type") == "app_mention"), "app_mention")[0]
    e = json.loads(g["body"])
    assert e["type"] == "event_callback" and e["event"]["text"] == "<@U0300BOT> deploy status?" and e["team_id"] == "T0ACME0001" and e["event_id"].startswith("Ev"), e
    assert verifier.is_valid(g["body"], g["headers"]["x-slack-request-timestamp"], g["headers"]["x-slack-signature"])
    assert not SignatureVerifier("another-secret").is_valid(g["body"], g["headers"]["x-slack-request-timestamp"], g["headers"]["x-slack-signature"])


# ---------------------------------------------------------------------------------------------- github
def gh_verified(g, secret=None):
    want = "sha256=" + hmac.new((secret or SECRETS["github-webhook-secret"]).encode(), g["body"].encode(), hashlib.sha256).hexdigest()
    return hmac.compare_digest(want, g["headers"]["x-hub-signature-256"])


def t_github(s):
    from github import Auth, Github, GithubException
    s.call("PUT", "/plugins/github/data/settings", {"rows": [{"webhook_url": hook_url("github")}]})
    g = Github(base_url=f"http://127.0.0.1:{s.APIS_PORT}/github", auth=Auth.Token("ghp_acmeTestToken0000000000000000000001"), retry=None)
    me = g.get_user()
    assert me.login == "emma-acme" and me.email == "emma.carter@acme.example.com", me
    repo = g.get_repo("acme/storefront")
    assert repo.full_name == "acme/storefront" and repo.default_branch == "main" and repo.id == 700100001, repo
    issue = repo.create_issue(title="Checkout button misaligned on Safari", body="Steps: open /cart on Safari 18")
    assert issue.number == 42 and issue.state == "open" and issue.title.startswith("Checkout"), issue
    ev = lambda kind, action: hooks("/github", lambda h: h["headers"]["x-github-event"] == kind and json.loads(h["body"]).get("action") == action)
    h = wait(lambda: ev("issues", "opened"), "issues.opened")[0]
    assert gh_verified(h) and not gh_verified(h, "wrong") and len(h["headers"]["x-github-delivery"]) == 36, h["headers"]
    assert json.loads(h["body"])["issue"]["number"] == 42 and json.loads(h["body"])["repository"]["full_name"] == "acme/storefront", h["body"]
    c = issue.create_comment("Reproduced on 18.1")
    assert c.body == "Reproduced on 18.1" and c.user.login == "emma-acme", c
    assert gh_verified(wait(lambda: ev("issue_comment", "created"), "issue_comment")[0])
    issue.edit(state="closed", state_reason="completed")
    assert issue.state == "closed", issue.state
    assert gh_verified(wait(lambda: ev("issues", "closed"), "issues.closed")[0])
    assert repo.get_issue(42).title == "Checkout button misaligned on Safari"
    pr = repo.create_pull(title="Fix Safari checkout button", body="Closes #42", head="fix/safari-button", base="main")
    assert pr.number == 43 and pr.head.ref == "fix/safari-button" and pr.base.ref == "main", pr
    assert json.loads(wait(lambda: ev("pull_request", "opened"), "pull_request")[0]["body"])["pull_request"]["number"] == 43
    run = repo.create_check_run(name="acme-ci / test", head_sha=pr.head.sha, status="in_progress")
    assert run.status == "in_progress" and run.name == "acme-ci / test", run
    run.edit(conclusion="success", output={"title": "All green", "summary": "212 tests passed"})
    assert run.conclusion == "success" and run.status == "completed", (run.status, run.conclusion)
    done = json.loads(wait(lambda: ev("check_run", "completed"), "check_run.completed")[0]["body"])
    assert done["check_run"]["conclusion"] == "success", done
    # Errors.
    for call, status, msg in [(lambda: g.get_repo("acme/nope"), 404, "Not Found"), (lambda: repo.create_issue(title=""), 422, None),
                              (lambda: Github(base_url=g._Github__requester.base_url, auth=Auth.Token("nope"), retry=None).get_user().login, 401, "Bad credentials")]:
        try:
            call()
            raise AssertionError("no error")
        except GithubException as e:
            assert e.status == status and (msg is None or e.data["message"] == msg), (e.status, e.data)
    st, v, hd = s.api("GET", "/github/repos/acme/storefront", None, {"Authorization": "token ghp_x"})
    assert st == 200 and hd["x-ratelimit-limit"] == "5000" and int(hd["x-ratelimit-reset"]) > time.time(), hd
    # Forced: the primary rate limit.
    s.call("POST", "/apis/github/force", {"answer": "rate_limited", "count": 1})
    st, v, hd = s.api("GET", "/github/user", None, {"Authorization": "token ghp_x"})
    assert st == 403 and hd["x-ratelimit-remaining"] == "0" and v["message"].startswith("API rate limit exceeded"), (st, v, hd)


# ---------------------------------------------------------------------------------------------- shopify
def shopify_verified(g, secret=None):
    want = base64.b64encode(hmac.new((secret or SECRETS["shopify-webhook-secret"]).encode(), g["body"].encode(), hashlib.sha256).digest()).decode()
    return hmac.compare_digest(want, g["headers"]["x-shopify-hmac-sha256"])


def t_shopify(s):
    s.call("PUT", "/plugins/shopify/data/settings", {"rows": [{"webhook_url": hook_url("shopify")}]})
    H = {"X-Shopify-Access-Token": "shpat_acme0000000000000000000000000001"}
    A = "/shopify/admin/api/2024-10"
    st, v, hd = s.api("GET", A + "/products.json", None, H)
    assert st == 200 and v["products"][0]["title"] == "Acme Trail Bottle" and hd["x-shopify-shop-api-call-limit"] == "1/40", (st, v, hd)
    st, v, _ = s.api("POST", A + "/products.json", {"product": {"title": "Acme Fleece", "vendor": "Acme", "variants": [{"price": "59.00", "sku": "ACME-FL-M"}]}}, H)
    assert st == 201 and v["product"]["title"] == "Acme Fleece" and v["product"]["variants"][0]["price"] == "59.00" and v["product"]["handle"] == "acme-fleece", v
    pid = v["product"]["id"]
    st, v, _ = s.api("GET", f"{A}/products/{pid}.json", None, H)
    assert st == 200 and v["product"]["id"] == pid, v
    g = wait(lambda: hooks("/shopify", lambda g: g["headers"]["x-shopify-topic"] == "products/create"), "products/create")[0]
    assert shopify_verified(g) and not shopify_verified(g, "wrong") and json.loads(g["body"])["id"] == pid, g["headers"]
    assert g["headers"]["x-shopify-shop-domain"] == "acme-store.myshopify.com" and g["headers"]["x-shopify-api-version"] == "2024-10", g["headers"]
    st, cu, _ = s.api("POST", A + "/customers.json", {"customer": {"first_name": "Lily", "last_name": "Parker", "email": "lily.parker@globex.example.com"}}, H)
    assert st == 201 and cu["customer"]["email"] == "lily.parker@globex.example.com", cu
    st, v, _ = s.api("POST", A + "/customers.json", {"customer": {"email": "lily.parker@globex.example.com"}}, H)
    assert st == 422 and v["errors"]["email"] == ["has already been taken"], v
    st, o, _ = s.api("POST", A + "/orders.json", {"order": {"email": "lily.parker@globex.example.com", "line_items": [{"variant_id": 4100000000101, "quantity": 2}]}}, H)
    assert st == 201 and o["order"]["name"] == "#1001" and o["order"]["total_price"] == "48.00" and o["order"]["line_items"][0]["title"] == "Acme Trail Bottle", o
    assert o["order"]["customer"]["id"] == cu["customer"]["id"], (o["order"]["customer"], cu["customer"]["id"])
    g = wait(lambda: hooks("/shopify", lambda g: g["headers"]["x-shopify-topic"] == "orders/create"), "orders/create")[0]
    assert shopify_verified(g) and json.loads(g["body"])["order_number"] == 1001, g
    st, v, _ = s.api("GET", f"{A}/orders/{o['order']['id']}.json", None, H)
    assert st == 200 and v["order"]["total_price"] == "48.00", v
    # Errors.
    st, v, _ = s.api("POST", A + "/products.json", {"product": {"title": ""}}, H)
    assert st == 422 and v["errors"]["title"] == ["can't be blank"], v
    st, v, _ = s.api("GET", A + "/orders/123.json", None, H)
    assert st == 404 and v["errors"] == "Not Found", v
    st, v, _ = s.api("GET", A + "/products.json", None, {"X-Shopify-Access-Token": "nope"})
    assert st == 401 and v["errors"].startswith("[API] Invalid API key"), v
    # Forced: throttled.
    s.call("POST", "/apis/shopify/force", {"answer": "throttled", "count": 1})
    st, v, hd = s.api("GET", A + "/products.json", None, H)
    assert st == 429 and hd["retry-after"] == "2.0" and hd["x-shopify-shop-api-call-limit"] == "40/40", (st, hd)
    # GraphQL.
    gq = lambda q, var=None: s.api("POST", A + "/graphql.json", {"query": q, "variables": var or {}}, H)
    st, v, _ = gq("{ shop { name myshopifyDomain } }")
    assert st == 200 and v["data"]["shop"]["name"] == "Acme Store" and v["extensions"]["cost"]["throttleStatus"]["maximumAvailable"] == 2000.0, v
    st, v, _ = gq("query { products(first: 10) { edges { node { id title } } } }")
    assert [e["node"]["title"] for e in v["data"]["products"]["edges"]] == ["Acme Trail Bottle", "Acme Canvas Tote"], v
    st, v, _ = gq("mutation productCreate($product: ProductCreateInput!) { productCreate(product: $product) { product { id title } userErrors { field message } } }", {"product": {"title": "Acme Cap"}})
    assert v["data"]["productCreate"]["product"]["title"] == "Acme Cap" and v["data"]["productCreate"]["userErrors"] == [], v
    st, v, _ = gq("mutation productCreate($product: ProductCreateInput!) { productCreate(product: $product) { product { id } userErrors { field message } } }", {"product": {"title": ""}})
    assert v["data"]["productCreate"]["userErrors"][0]["field"] == ["title"], v
    st, v, _ = gq("query order($id: ID!) { order(id: $id) { name } }", {"id": o["order"]["admin_graphql_api_id"]})
    assert v["data"]["order"]["name"] == "#1001", v


# ---------------------------------------------------------------------------------------------- hubspot
def t_hubspot(s):
    from hubspot import HubSpot
    from hubspot.crm.contacts import SimplePublicObjectInputForCreate, SimplePublicObjectInput, PublicObjectSearchRequest
    from hubspot.crm.contacts.exceptions import ApiException
    from hubspot.utils.signature import Signature
    target = hook_url("hubspot", "/webhooks")
    s.call("PUT", "/plugins/hubspot/data/settings", {"rows": [{"webhook_url": target, "portal_id": 62515001, "app_id": 1170001, "subscription_id": 2340001}]})
    hs = HubSpot(access_token="pat-na1-acme-0000-0000-000000000001", host=f"http://127.0.0.1:{s.APIS_PORT}/hubspot")
    c = hs.crm.contacts.basic_api.create(SimplePublicObjectInputForCreate(properties={"email": "lily.parker@globex.example.com", "firstname": "Lily", "lastname": "Parker"}))
    assert c.id and c.properties["email"] == "lily.parker@globex.example.com" and c.properties["hs_object_id"] == c.id, c
    got = hs.crm.contacts.basic_api.get_by_id(c.id)
    assert got.properties["firstname"] == "Lily", got
    up = hs.crm.contacts.basic_api.update(c.id, SimplePublicObjectInput(properties={"lifecyclestage": "customer"}))
    assert up.properties["lifecyclestage"] == "customer" and up.properties["firstname"] == "Lily", up
    assert hs.crm.contacts.basic_api.get_by_id(c.id).properties["lifecyclestage"] == "customer"
    res = hs.crm.contacts.search_api.do_search(PublicObjectSearchRequest(filter_groups=[{"filters": [{"propertyName": "email", "operator": "EQ", "value": "lily.parker@globex.example.com"}]}]))
    assert res.total == 1 and res.results[0].id == c.id, res
    assert hs.crm.contacts.search_api.do_search(PublicObjectSearchRequest(filter_groups=[{"filters": [{"propertyName": "email", "operator": "EQ", "value": "nobody@example.com"}]}])).total == 0
    from hubspot.crm.companies import SimplePublicObjectInputForCreate as CompanyIn
    from hubspot.crm.deals import SimplePublicObjectInputForCreate as DealIn
    co = hs.crm.companies.basic_api.create(CompanyIn(properties={"name": "Globex", "domain": "globex.example.com"}))
    deal = hs.crm.deals.basic_api.create(DealIn(properties={"dealname": "Globex renewal", "amount": "12000", "dealstage": "contractsent"}))
    assert co.properties["name"] == "Globex" and deal.properties["dealstage"] == "contractsent" and deal.properties["pipeline"] == "default", (co, deal)
    a = hs.crm.associations.v4.basic_api.create_default("contacts", c.id, "companies", co.id)
    assert a.status == "COMPLETE" and a.results[0].association_spec.association_type_id == 279, a
    page = hs.crm.associations.v4.basic_api.get_page("contacts", c.id, "companies")
    assert str(page.results[0].to_object_id) == co.id and page.results[0].association_types[0].type_id == 279, page
    # Errors.
    for call, status, cat in [(lambda: hs.crm.contacts.basic_api.create(SimplePublicObjectInputForCreate(properties={"email": "lily.parker@globex.example.com"})), 409, "CONFLICT"),
                              (lambda: hs.crm.contacts.basic_api.get_by_id("999"), 404, "OBJECT_NOT_FOUND"),
                              (lambda: hs.crm.contacts.basic_api.create(SimplePublicObjectInputForCreate(properties={"email": "not-an-email"})), 400, "VALIDATION_ERROR"),
                              (lambda: HubSpot(access_token="nope", host=f"http://127.0.0.1:{s.APIS_PORT}/hubspot").crm.contacts.basic_api.get_by_id(c.id), 401, "INVALID_AUTHENTICATION")]:
        try:
            call()
            raise AssertionError("no error")
        except ApiException as e:
            assert e.status == status and json.loads(e.body)["category"] == cat, (e.status, e.body)
    # Forced: 429 with HubSpot's body.
    s.call("POST", "/apis/hubspot/force", {"answer": "rate_limited", "count": 1})
    st, v, _ = s.api("GET", f"/hubspot/crm/v3/objects/contacts/{c.id}", None, {"Authorization": "Bearer pat-na1-x"})
    assert st == 429 and v["errorType"] == "RATE_LIMIT" and v["policyName"] == "SECONDLY", v
    # Webhooks v3: an array of events, signed over method + URL + body + timestamp.
    evs = wait(lambda: (lambda x: x if len(x) >= 4 else None)(hooks("/hubspot/webhooks")), "hubspot webhooks")
    kinds = sorted(json.loads(g["body"])[0]["subscriptionType"] for g in evs)
    assert kinds == ["company.creation", "contact.creation", "contact.propertyChange", "deal.creation"], kinds
    for g in evs:
        h = g["headers"]
        assert Signature.is_valid(signature=h["x-hubspot-signature-v3"], client_secret=SECRETS["hubspot-client-secret"], request_body=g["body"], http_uri=target, http_method="POST", signature_version="v3", timestamp=h["x-hubspot-request-timestamp"]), h
        assert not Signature.is_valid(signature=h["x-hubspot-signature-v3"], client_secret="wrong", request_body=g["body"], http_uri=target, http_method="POST", signature_version="v3", timestamp=h["x-hubspot-request-timestamp"])
    pc = [json.loads(g["body"])[0] for g in evs if json.loads(g["body"])[0]["subscriptionType"] == "contact.propertyChange"][0]
    assert pc["propertyName"] == "lifecyclestage" and pc["propertyValue"] == "customer" and pc["objectId"] == int(c.id) and pc["portalId"] == 62515001, pc


# ---------------------------------------------------------------------------------------------- sentry
def t_sentry(s):
    import sentry_sdk
    key, project = "7c3d0e5a2f1b4c6d8e9f0a1b2c3d4e5f", 4508000000000101
    sentry_sdk.init(dsn=f"http://{key}@127.0.0.1:{s.APIS_PORT}/sentry/{project}", environment="staging", release="acme-web@2026.11.15", default_integrations=False, traces_sample_rate=1.0)
    try:
        1 / 0
    except ZeroDivisionError as e:
        eid = sentry_sdk.capture_exception(e)
    mid = sentry_sdk.capture_message("Checkout queue is backing up", level="warning")
    with sentry_sdk.start_transaction(op="task", name="nightly-invoices"):
        pass
    sentry_sdk.flush(5)
    # Read back what the app reported (gzip envelopes, decompressed on arrival).
    W = {"Authorization": "Bearer sntrys_acme_token"}
    st, ev, _ = s.api("GET", f"/sentry/api/0/projects/acme/acme-web/events/{eid}/", None, W)
    assert st == 200 and ev["eventID"] == eid and ev["title"] == "ZeroDivisionError: division by zero" and ev["environment"] == "staging" and ev["release"] == "acme-web@2026.11.15", ev
    assert ev["exception"]["values"][-1]["type"] == "ZeroDivisionError" and ev["sdk"]["name"] == "sentry.python", ev
    st, ev, _ = s.api("GET", f"/sentry/api/0/projects/acme/acme-web/events/{mid}/", None, W)
    assert st == 200 and ev["title"] == "Checkout queue is backing up" and ev["level"] == "warning", ev
    st, kept, _ = s.api("GET", "/sentry/stuntman/events")
    assert eid in kept and mid in kept and any("nightly-invoices" in v["body"] for v in kept.values() if isinstance(v["body"], str)), list(kept)
    # The legacy store endpoint, with the key as a query parameter.
    st, v, _ = s.api("POST", f"/sentry/api/{project}/store/?sentry_key={key}&sentry_version=7", {"event_id": "a" * 32, "message": "store endpoint works", "level": "info", "platform": "javascript"})
    assert st == 200 and v["id"] == "a" * 32, v
    st, ev, _ = s.api("GET", f"/sentry/api/0/projects/acme/acme-web/events/{'a' * 32}/", None, W)
    assert st == 200 and ev["title"] == "store endpoint works", ev
    # Errors.
    st, v, _ = s.api("POST", f"/sentry/api/{project}/store/", {"message": "x"})
    assert st == 401 and v["detail"] == "missing authorization information", v
    st, v, _ = s.api("POST", f"/sentry/api/{project}/store/?sentry_key=wrong", {"message": "x"})
    assert st == 403, v
    st, v, _ = s.api("GET", "/sentry/api/0/projects/acme/acme-web/events/" + "f" * 32 + "/", None, W)
    assert st == 404, v
    # Forced: 429 with Sentry's rate-limit headers; the SDK then drops errors instead of sending them.
    s.call("POST", "/apis/sentry/force", {"answer": "rate_limited", "count": 1})
    st, v, hd = s.api("POST", f"/sentry/api/{project}/store/?sentry_key={key}", {"message": "x"})
    assert st == 429 and hd["retry-after"] == "60" and hd["x-sentry-rate-limits"].startswith("60:error"), hd
    s.call("POST", "/apis/sentry/force", {"answer": "rate_limited", "count": 1})
    sentry_sdk.capture_message("first, refused")
    sentry_sdk.flush(5)
    later = sentry_sdk.capture_message("second, dropped by the SDK")
    sentry_sdk.flush(5)
    st, kept, _ = s.api("GET", "/sentry/stuntman/events")
    assert later not in kept, "the SDK ignored X-Sentry-Rate-Limits"
    sentry_sdk.get_client().close()


# ---------------------------------------------------------------------------------------------- auth0
def t_auth0(s):
    from auth0.authentication import GetToken, Users
    from auth0.authentication.exceptions import Auth0Error
    from auth0.authentication.token_verifier import AsymmetricSignatureVerifier, TokenVerifier
    from auth0.management.client import Auth0 as ManagementApi  # the generated client: it takes a base_url
    import jwt as pyjwt
    dom = f"127.0.0.1:{s.APIS_PORT}/auth0"
    iss = f"http://{dom}/"
    web, m2m = "AcmeWebApp0000000000000000000001", "AcmeBackend000000000000000000002"
    # Client credentials: an RS256 JWT that verifies against the tenant's JWKS.
    tok = GetToken(dom, m2m, client_secret="acme-backend-secret", protocol="http").client_credentials("https://acme.us.auth0.com/api/v2/")
    assert tok["token_type"] == "Bearer" and tok["expires_in"] == 86400, tok
    jwks = pyjwt.PyJWKClient(iss + ".well-known/jwks.json")
    claims = pyjwt.decode(tok["access_token"], jwks.get_signing_key_from_jwt(tok["access_token"]).key, algorithms=["RS256"], audience="https://acme.us.auth0.com/api/v2/", issuer=iss)
    assert claims["sub"] == m2m + "@clients" and claims["gty"] == "client-credentials", claims
    st, oc, _ = s.api("GET", "/auth0/.well-known/openid-configuration")
    assert oc["issuer"] == iss and oc["jwks_uri"] == iss + ".well-known/jwks.json", oc
    # Password realm: an ID token the SDK's TokenVerifier accepts, and /userinfo.
    t = GetToken(dom, web, protocol="http").login("emma.carter@acme.example.com", "Acme-Passw0rd!", realm="Username-Password-Authentication", scope="openid profile email")
    idt = TokenVerifier(signature_verifier=AsymmetricSignatureVerifier(iss + ".well-known/jwks.json"), issuer=iss, audience=web).verify(t["id_token"])
    assert idt["email"] == "emma.carter@acme.example.com" and idt["sub"] == "auth0|6650a1b2c3d4e5f600000001", idt
    me = Users(dom, protocol="http").userinfo(t["access_token"])
    assert me["email"] == "emma.carter@acme.example.com" and me["sub"] == idt["sub"], me
    # Errors.
    for call, err in [(lambda: GetToken(dom, web, protocol="http").login("emma.carter@acme.example.com", "wrong", realm="Username-Password-Authentication"), "invalid_grant"),
                      (lambda: GetToken(dom, m2m, client_secret="wrong", protocol="http").client_credentials("https://acme.us.auth0.com/api/v2/"), "access_denied"),
                      (lambda: GetToken(dom, web, protocol="http").login("jack.morgan@globex.example.com", "Globex-Passw0rd!", realm="Username-Password-Authentication"), "unauthorized")]:
        try:
            call()
            raise AssertionError("no error " + err)
        except Auth0Error as e:
            assert e.error_code == err, (e.status_code, e.error_code, e.message)
    st, v, _ = s.api("GET", "/auth0/userinfo", None, {"Authorization": "Bearer nope"})
    assert st == 401 and v == b"Unauthorized", (st, v)
    # Forced: too_many_requests on the token endpoint.
    s.call("POST", "/apis/auth0/force", {"answer": "too_many_requests", "count": 1})
    try:
        GetToken(dom, m2m, client_secret="acme-backend-secret", protocol="http").client_credentials("https://acme.us.auth0.com/api/v2/")
        raise AssertionError("not forced")
    except Exception as e:
        assert getattr(e, "status_code", None) == 429 or "429" in str(e) or "too_many_requests" in str(e), e
    # Management API v2.
    mc = ManagementApi(base_url=iss + "api/v2", token=tok["access_token"])
    u = mc.users.create(connection="Username-Password-Authentication", email="lily.parker@globex.example.com", password="Globex-New-2026!", name="Lily Parker")
    assert u.user_id.startswith("auth0|") and u.email == "lily.parker@globex.example.com", u
    assert mc.users.get(u.user_id).name == "Lily Parker"
    up = mc.users.update(u.user_id, user_metadata={"plan": "pro"})
    assert up.user_metadata == {"plan": "pro"} and up.name == "Lily Parker", up
    assert [x.user_id for x in mc.users.list_users_by_email(email="lily.parker@globex.example.com")] == [u.user_id]
    roles = list(mc.roles.list())
    assert [r.name for r in roles] == ["admin", "editor", "viewer"], roles
    mc.users.roles.assign(u.user_id, roles=["rol_AcmeEditor0000002"])
    assert [r.name for r in mc.users.roles.list(u.user_id)] == ["editor"]
    # The new user signs in.
    t2 = GetToken(dom, web, protocol="http").login("lily.parker@globex.example.com", "Globex-New-2026!", realm="Username-Password-Authentication", scope="openid email")
    assert Users(dom, protocol="http").userinfo(t2["access_token"])["sub"] == u.user_id
    try:
        mc.users.create(connection="Username-Password-Authentication", email="lily.parker@globex.example.com", password="Globex-New-2026!")
        raise AssertionError("created twice")
    except Exception as e:
        assert getattr(e, "status_code", None) == 409, e
    mc.users.delete(u.user_id)
    try:
        mc.users.get(u.user_id)
        raise AssertionError("still there")
    except Exception as e:
        assert getattr(e, "status_code", None) == 404 and e.body["errorCode"] == "inexistent_user", e
    st, v, _ = s.api("GET", "/auth0/api/v2/roles", None, {"Authorization": "Bearer " + t["access_token"]})
    assert st == 401 and v["message"] == "Invalid token", v
    s.call("POST", "/apis/auth0/force", {"answer": "mgmt_rate_limited", "count": 1})
    st, v, hd = s.api("GET", "/auth0/api/v2/roles", None, {"Authorization": "Bearer " + tok["access_token"]})
    assert st == 429 and v["errorCode"] == "too_many_requests" and hd["x-ratelimit-remaining"] == "0", v


# ---------------------------------------------------------------------------------------------- adyen
def t_adyen(s):
    import Adyen
    from Adyen.util import is_valid_hmac_notification
    hook = {"merchant_account": "AcmeECOM", "webhook_url": hook_url("adyen"), "webhook_user": "acme-adyen-hooks", "webhook_password": "hooks-pass-2026", "hmac_key": ADYEN_HMAC}
    s.call("PUT", "/plugins/adyen/data/settings", {"rows": [hook]})
    ady = Adyen.Adyen()
    ady.client.xapikey = "AQEyhmfxacme0000000000000000000001"
    ady.client.platform = "test"
    base = f"http://127.0.0.1:{s.APIS_PORT}/adyen/v71"
    pay, mods = ady.checkout.payments_api, ady.checkout.modifications_api
    pay.baseUrl = mods.baseUrl = base
    amount = {"currency": "EUR", "value": 2500}
    r = pay.payment_methods({"merchantAccount": "AcmeECOM", "countryCode": "NL", "amount": amount})
    assert [m["type"] for m in r.message["paymentMethods"]][:2] == ["scheme", "ideal"], r.message
    ses = pay.sessions({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-ORDER-1001", "returnUrl": "https://shop.acme.example.com/checkout/return", "countryCode": "NL"})
    assert ses.status_code == 201 and ses.message["id"].startswith("CS") and ses.message["sessionData"], ses.message
    card = {"type": "scheme", "encryptedCardNumber": "test_4111111111111111", "encryptedExpiryMonth": "test_03", "encryptedExpiryYear": "test_2030", "encryptedSecurityCode": "test_737", "holderName": "Emma Carter"}
    r = pay.payments({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-ORDER-1001", "paymentMethod": card, "returnUrl": "https://shop.acme.example.com/checkout/return"}, idempotency_key="order-1001-attempt-1")
    assert r.message["resultCode"] == "Authorised" and len(r.message["pspReference"]) == 16 and r.message["paymentMethod"]["brand"] == "visa", r.message
    psp = r.message["pspReference"]
    again = pay.payments({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-ORDER-1001", "paymentMethod": card, "returnUrl": "https://shop.acme.example.com/checkout/return"}, idempotency_key="order-1001-attempt-1")
    assert again.message["pspReference"] == psp, again.message
    note = lambda code, ref=None: [json.loads(g["body"]) for g in hooks("/adyen") if json.loads(g["body"])["notificationItems"][0]["NotificationRequestItem"]["eventCode"] == code
                                   and (ref is None or json.loads(g["body"])["notificationItems"][0]["NotificationRequestItem"]["pspReference"] == ref)]
    n = wait(lambda: note("AUTHORISATION", psp), "AUTHORISATION")
    assert len(n) == 1, "the idempotent replay sent a second webhook"
    item = n[0]["notificationItems"][0]["NotificationRequestItem"]
    assert item["success"] == "true" and item["merchantReference"] == "ACME-ORDER-1001" and is_valid_hmac_notification(item, ADYEN_HMAC), item
    assert not is_valid_hmac_notification(item, "00" * 32)
    g = [g for g in hooks("/adyen") if psp in g["body"]][0]
    assert g["headers"]["authorization"] == "Basic " + base64.b64encode(b"acme-adyen-hooks:hooks-pass-2026").decode(), g["headers"]
    # Refused by holder name.
    r = pay.payments({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-ORDER-1002", "paymentMethod": dict(card, holderName="NOT_ENOUGH_BALANCE")})
    assert r.message["resultCode"] == "Refused" and r.message["refusalReason"] == "Not enough balance", r.message
    item = wait(lambda: note("AUTHORISATION", r.message["pspReference"]), "refused AUTHORISATION")[0]["notificationItems"][0]["NotificationRequestItem"]
    assert item["success"] == "false" and is_valid_hmac_notification(item, ADYEN_HMAC), item
    # 3D Secure: redirect, challenge page, details.
    r = pay.payments({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-ORDER-1003", "paymentMethod": dict(card, encryptedCardNumber="test_4212345678910006"), "returnUrl": "https://shop.acme.example.com/checkout/return"})
    assert r.message["resultCode"] == "RedirectShopper" and r.message["action"]["type"] == "redirect", r.message
    q = r.message["action"]["url"].split("?", 1)[1]
    st, _, hd = s.api("GET", "/adyen/checkoutshopper/threeDS/redirect?" + q)
    assert st == 302 and hd["location"].startswith("https://shop.acme.example.com/checkout/return?redirectResult="), (st, hd)
    rr = urllib.parse.parse_qs(hd["location"].split("?", 1)[1])["redirectResult"][0]
    d = pay.payments_details({"details": {"redirectResult": rr}})
    assert d.message["resultCode"] == "Authorised" and d.message["pspReference"] == r.message["pspReference"], d.message
    # Modifications and their webhooks.
    c = mods.capture_authorised_payment({"merchantAccount": "AcmeECOM", "amount": amount, "reference": "ACME-CAP-1001"}, psp)
    assert c.status_code == 201 and c.message["status"] == "received" and c.message["paymentPspReference"] == psp, c.message
    item = wait(lambda: note("CAPTURE"), "CAPTURE")[0]["notificationItems"][0]["NotificationRequestItem"]
    assert item["originalReference"] == psp and item["success"] == "true" and is_valid_hmac_notification(item, ADYEN_HMAC), item
    mods.refund_captured_payment({"merchantAccount": "AcmeECOM", "amount": {"currency": "EUR", "value": 99999}, "reference": "ACME-REF-1"}, psp)
    item = wait(lambda: note("REFUND"), "REFUND")[0]["notificationItems"][0]["NotificationRequestItem"]
    assert item["success"] == "false" and item["reason"] == "Insufficient balance on payment" and is_valid_hmac_notification(item, ADYEN_HMAC), item
    # Errors.
    from Adyen.exceptions import AdyenAPIResponseError
    for req, status, code in [({"merchantAccount": "AcmeECOM", "amount": amount, "paymentMethod": card}, 422, "130"), ({"merchantAccount": "GlobexECOM", "amount": amount, "reference": "x", "paymentMethod": card}, 403, "901")]:
        try:
            pay.payments(req)
            raise AssertionError("no error")
        except AdyenAPIResponseError as e:
            assert e.status_code == status and e.error_code == code, (e.status_code, e.error_code, e.raw_response)
    st, v, _ = s.api("POST", "/adyen/v71/payments", {"merchantAccount": "AcmeECOM"})
    assert st == 401 and v["errorType"] == "security", v
    # Forced.
    s.call("POST", "/apis/adyen/force", {"answer": "too_many_requests", "count": 1})
    st, v, _ = s.api("POST", "/adyen/v71/paymentMethods", {"merchantAccount": "AcmeECOM"}, {"X-API-Key": "AQEyhmfxacme0000"})
    assert st == 429 and v["status"] == 429, v


# ---------------------------------------------------------------------------------------------- paypal
def t_paypal(s):
    from paypalserversdk.configuration import Configuration, Environment, Server
    from paypalserversdk.exceptions.error_exception import ErrorException
    from paypalserversdk.http.auth.o_auth_2 import ClientCredentialsAuthCredentials
    from paypalserversdk.models.amount_with_breakdown import AmountWithBreakdown
    from paypalserversdk.models.checkout_payment_intent import CheckoutPaymentIntent
    from paypalserversdk.models.order_request import OrderRequest
    from paypalserversdk.models.purchase_unit_request import PurchaseUnitRequest
    from paypalserversdk.models.refund_request import RefundRequest
    from paypalserversdk.models.money import Money
    from paypalserversdk.paypal_serversdk_client import PaypalServersdkClient
    settings = {"merchant_email": "merchant@acme.example.com", "merchant_id": "ACMEMERCHANT01", "webhook_url": hook_url("paypal"), "webhook_id": "8PT597110X687430LKGECATA", "signing_key": SECRETS["paypal-webhook-key"]}
    s.call("PUT", "/plugins/paypal/data/settings", {"rows": [settings]})
    Configuration.environments[Environment.SANDBOX][Server.DEFAULT] = f"http://127.0.0.1:{s.APIS_PORT}/paypal"
    cid, csec = "AcmeSandboxClientId000000000000000000000000000000000000000000000001", "EAcmeSandboxSecret0000000000000000000000000000000000000000000000001"
    pp = PaypalServersdkClient(client_credentials_auth_credentials=ClientCredentialsAuthCredentials(o_auth_client_id=cid, o_auth_client_secret=csec), environment=Environment.SANDBOX)
    order_req = lambda intent, value="100.00": OrderRequest(intent=intent, purchase_units=[PurchaseUnitRequest(reference_id="ACME-ORDER-2001", amount=AmountWithBreakdown(currency_code="USD", value=value))])
    o = pp.orders.create_order({"body": order_req(CheckoutPaymentIntent.CAPTURE), "paypal_request_id": "acme-2001", "prefer": "return=representation"}).body
    assert o.status == "CREATED" and len(o.id) == 17 and [l.rel for l in o.links][:2] == ["self", "approve"], o
    again = pp.orders.create_order({"body": order_req(CheckoutPaymentIntent.CAPTURE), "paypal_request_id": "acme-2001"}).body
    assert again.id == o.id, (again.id, o.id)
    # Capture before approval: ORDER_NOT_APPROVED.
    try:
        pp.orders.capture_order({"id": o.id, "prefer": "return=representation"})
        raise AssertionError("captured unapproved")
    except ErrorException as e:
        assert e.response_code == 422 and json.loads(e.response.text)["details"][0]["issue"] == "ORDER_NOT_APPROVED", e.response.text
    # The buyer approves at the approve link and comes back to the return URL.
    st, _, hd = s.api("GET", "/paypal/checkoutnow?token=" + o.id)
    assert st == 302 and f"token={o.id}&PayerID=QYR5Z8XDVJNXQ" in hd["location"], (st, hd)
    assert pp.orders.get_order({"id": o.id}).body.status == "APPROVED"
    # Negative testing: the mock header declines the instrument.
    try:
        pp.orders.capture_order({"id": o.id, "paypal_mock_response": json.dumps({"mock_application_codes": "INSTRUMENT_DECLINED"})})
        raise AssertionError("not declined")
    except ErrorException as e:
        assert e.response_code == 422 and json.loads(e.response.text)["details"][0]["issue"] == "INSTRUMENT_DECLINED", e.response.text
    cap = pp.orders.capture_order({"id": o.id, "prefer": "return=representation"}).body
    c = cap.purchase_units[0].payments.captures[0]
    assert cap.status == "COMPLETED" and c.status == "COMPLETED" and c.amount.value == "100.00" and cap.payer.email_address == "sb-buyer-emma@personal.example.com", cap
    # Refunds: part, then too much.
    r = pp.payments.refund_captured_payment({"capture_id": c.id, "body": RefundRequest(amount=Money(currency_code="USD", value="25.00"))}).body
    assert r.status == "COMPLETED" and r.amount.value == "25.00", r
    try:
        pp.payments.refund_captured_payment({"capture_id": c.id, "body": RefundRequest(amount=Money(currency_code="USD", value="250.00"))})
        raise AssertionError("refunded too much")
    except ErrorException as e:
        assert e.response_code == 422 and "REFUND_AMOUNT_EXCEEDED" in e.response.text, e.response.text
    # Authorize, then capture the authorization.
    a = pp.orders.create_order({"body": order_req(CheckoutPaymentIntent.AUTHORIZE, "40.00")}).body
    s.api("GET", "/paypal/checkoutnow?token=" + a.id)
    au = pp.orders.authorize_order({"id": a.id, "prefer": "return=representation"}).body
    auth = au.purchase_units[0].payments.authorizations[0]
    assert au.status == "COMPLETED" and auth.status == "CREATED", au
    ac = pp.payments.capture_authorized_payment({"authorization_id": auth.id}).body
    assert ac.status == "COMPLETED" and ac.amount.value == "40.00", ac
    # Webhooks, verified the way PayPal tells apps to: POST /v1/notifications/verify-webhook-signature.
    tok = s.api("POST", "/paypal/v1/oauth2/token", b"grant_type=client_credentials", {"Authorization": "Basic " + base64.b64encode(f"{cid}:{csec}".encode()).decode(), "Content-Type": "application/x-www-form-urlencoded"})[1]["access_token"]
    def verify(g, webhook_id=settings["webhook_id"]):
        h = g["headers"]
        req = {"auth_algo": h["paypal-auth-algo"], "cert_url": h["paypal-cert-url"], "transmission_id": h["paypal-transmission-id"], "transmission_sig": h["paypal-transmission-sig"],
               "transmission_time": h["paypal-transmission-time"], "webhook_id": webhook_id, "webhook_event": json.loads(g["body"])}
        st, v, _ = s.api("POST", "/paypal/v1/notifications/verify-webhook-signature", req, {"Authorization": "Bearer " + tok})
        assert st == 200, v
        return v["verification_status"]
    kinds = lambda: {json.loads(g["body"])["event_type"]: g for g in hooks("/paypal")}
    got = wait(lambda: (lambda k: k if len(k) >= 4 else None)(kinds()), "paypal webhooks")
    assert set(got) == {"CHECKOUT.ORDER.APPROVED", "PAYMENT.CAPTURE.COMPLETED", "PAYMENT.CAPTURE.REFUNDED", "PAYMENT.AUTHORIZATION.CREATED"}, set(got)
    ev = json.loads(got["PAYMENT.CAPTURE.COMPLETED"]["body"])
    assert ev["resource"]["id"] == c.id and ev["id"].startswith("WH-") and ev["resource_type"] == "capture", ev
    for g in got.values():
        assert verify(g) == "SUCCESS", g["headers"]
    assert verify(got["PAYMENT.CAPTURE.COMPLETED"], "WRONGWEBHOOKID") == "FAILURE"
    tampered = dict(got["PAYMENT.CAPTURE.COMPLETED"], headers=dict(got["PAYMENT.CAPTURE.COMPLETED"]["headers"], **{"paypal-transmission-id": "00000000-0000-0000-0000-000000000000"}))
    assert verify(tampered) == "FAILURE"
    # Errors.
    try:
        pp.orders.get_order({"id": "0NOSUCHORDER00000"})
        raise AssertionError("found")
    except ErrorException as e:
        assert e.response_code == 404 and json.loads(e.response.text)["name"] == "RESOURCE_NOT_FOUND", e.response.text
    st, v, _ = s.api("POST", "/paypal/v1/oauth2/token", b"grant_type=client_credentials", {"Authorization": "Basic " + base64.b64encode(b"x:y").decode(), "Content-Type": "application/x-www-form-urlencoded"})
    assert st == 401 and v["error"] == "invalid_client", v
    st, v, _ = s.api("GET", "/paypal/v2/checkout/orders/" + o.id, None, {"Authorization": "Bearer nope"})
    assert st == 401 and v["name"] == "AUTHENTICATION_FAILURE", v
    # Forced.
    s.call("POST", "/apis/paypal/force", {"answer": "rate_limit_reached", "count": 1})
    st, v, _ = s.api("GET", "/paypal/v2/checkout/orders/" + o.id, None, {"Authorization": "Bearer " + tok})
    assert st == 429 and v["name"] == "RATE_LIMIT_REACHED", v


def main():
    only = sys.argv[1:] or NAMES
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", HOOK), Hooks)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    with Stuntman("stuntman-global-") as s:
        for name in only:
            text = open(os.path.join(PLUGINS, name + ".yaml")).read()
            v = s.call("POST", f"/plugins/{name}/validate", {"yaml": text})
            assert v["ok"] and v["errors"] == [], (name, v)
            s.call("POST", "/plugins", {"plugin": name})
        env = [r for r, _, f in os.walk(s.data) if only[0] + ".yaml" in f][0]
        with open(os.path.join(os.path.dirname(env), "secrets.yaml"), "a") as f:
            f.write("\n" + "".join(f"{k}: {v}\n" for k, v in SECRETS.items()))
        for name in only:
            globals()["t_" + name.replace("-", "_")](s)
            print("ok", name)
    print("e2e_plugins_global: ok")


if __name__ == "__main__":
    main()
