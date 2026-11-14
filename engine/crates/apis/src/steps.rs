//! Browser steps (A6): the pages a plugin declares (authorize with a person and a code) and push
//! approvals (the app polls while someone approves "on the phone"). Each opened page or push is a
//! step; authorizing gives a one-time grant the app exchanges at a `grant: authorization_code`
//! route (PKCE S256 checked), and the token it gets carries the person (auth.person).

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64U, Engine};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Map, Value};
use sha2::Digest;
use stuntman_core::{util, ApiError, Ctx};

use crate::{
    engine::{render_answer, Outcome, Req},
    expr,
    plugin::{Field, Page, Plugin, Route},
    store,
};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS api_steps(
  id TEXT PRIMARY KEY, at INTEGER NOT NULL, service TEXT NOT NULL, page INTEGER NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL,
  params TEXT NOT NULL DEFAULT '{}', person TEXT NOT NULL DEFAULT '', grant_code TEXT NOT NULL DEFAULT '', used INTEGER NOT NULL DEFAULT 0,
  expires INTEGER NOT NULL DEFAULT 0, tries INTEGER NOT NULL DEFAULT 0, by_who TEXT NOT NULL DEFAULT '', done_at INTEGER, log TEXT NOT NULL DEFAULT '[]');
CREATE INDEX IF NOT EXISTS api_steps_grant ON api_steps(grant_code);
";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let (params, log): (String, String) = (r.get(6)?, r.get(14)?);
    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "service": r.get::<_, String>(2)?, "page": r.get::<_, i64>(3)?, "kind": r.get::<_, String>(4)?,
        "status": r.get::<_, String>(5)?, "params": serde_json::from_str::<Value>(&params).unwrap_or(json!({})), "person": r.get::<_, String>(7)?,
        "grant": r.get::<_, String>(8)?, "used": r.get::<_, i64>(9)? != 0, "expires": r.get::<_, i64>(10)?, "tries": r.get::<_, i64>(11)?,
        "by": r.get::<_, String>(12)?, "done_at": r.get::<_, Option<i64>>(13)?, "log": serde_json::from_str::<Value>(&log).unwrap_or(json!([]))}))
}
const COLS: &str = "id, at, service, page, kind, status, params, person, grant_code, used, expires, tries, by_who, done_at, log";

fn line(text: &str) -> Value {
    json!({"at": util::now_ms(), "text": text})
}

async fn create(ctx: &Ctx, service: &str, page: usize, kind: &str, params: Value, person: &str, ttl_ms: i64) -> Result<String, ApiError> {
    let id = util::id("stp");
    let (i, s, k, pe) = (id.clone(), service.to_string(), kind.to_string(), person.to_string());
    let first = if kind == "push" { "push approval sent to the phone" } else { "page opened" };
    let log = json!([line(first)]).to_string();
    ctx.db.call(move |c| c.execute(&format!("INSERT INTO api_steps({COLS}) VALUES(?1,?2,?3,?4,?5,'pending',?6,?7,'',0,?8,0,'',NULL,?9)"),
        params![i, util::now_ms(), s, page as i64, k, params.to_string(), pe, util::now_ms() + ttl_ms, log]).map(|_| ())).await?;
    ctx.live.send("apis.step", &id, "created");
    Ok(id)
}

/// A step; a pending one past its time reads (and is saved) as expired.
pub async fn get(ctx: &Ctx, id: &str) -> Result<Option<Value>, ApiError> {
    let i = id.to_string();
    let v = ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM api_steps WHERE id=?1"), [&i], row).optional()).await?;
    match v {
        Some(s) if s["status"] == "pending" && s["expires"].as_i64().unwrap_or(0) < util::now_ms() => {
            update(ctx, id, "expired", None, None, "", "nobody answered in time").await?;
            Ok(get_raw(ctx, id).await?)
        }
        x => Ok(x),
    }
}

async fn get_raw(ctx: &Ctx, id: &str) -> Result<Option<Value>, ApiError> {
    let i = id.to_string();
    Ok(ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM api_steps WHERE id=?1"), [&i], row).optional()).await?)
}

async fn update(ctx: &Ctx, id: &str, status: &str, person: Option<String>, grant: Option<String>, by: &str, note: &str) -> Result<(), ApiError> {
    let (i, st, by, note) = (id.to_string(), status.to_string(), by.to_string(), note.to_string());
    ctx.db.call(move |c| {
        let cur: String = c.query_row("SELECT log FROM api_steps WHERE id=?1", [&i], |r| r.get(0)).optional()?.unwrap_or("[]".into());
        let mut log: Vec<Value> = serde_json::from_str(&cur).unwrap_or_default();
        if !note.is_empty() {
            log.push(line(&note));
        }
        let done = if st == "pending" { None } else { Some(util::now_ms()) };
        c.execute("UPDATE api_steps SET status=?2, person=COALESCE(?3, person), grant_code=COALESCE(?4, grant_code), by_who=CASE WHEN ?5='' THEN by_who ELSE ?5 END, done_at=COALESCE(?6, done_at), log=?7 WHERE id=?1",
            params![i, st, person, grant, by, done, json!(log).to_string()]).map(|_| ())
    }).await?;
    ctx.live.send("apis.step", id, "updated");
    Ok(())
}

async fn note(ctx: &Ctx, id: &str, text: &str, tries: Option<i64>) -> Result<(), ApiError> {
    let (i, t) = (id.to_string(), text.to_string());
    ctx.db.call(move |c| {
        let cur: String = c.query_row("SELECT log FROM api_steps WHERE id=?1", [&i], |r| r.get(0)).optional()?.unwrap_or("[]".into());
        let mut log: Vec<Value> = serde_json::from_str(&cur).unwrap_or_default();
        log.push(line(&t));
        c.execute("UPDATE api_steps SET log=?2, tries=COALESCE(?3, tries) WHERE id=?1", params![i, json!(log).to_string(), tries]).map(|_| ())
    }).await?;
    ctx.live.send("apis.step", id, "updated");
    Ok(())
}

pub async fn list(ctx: &Ctx, service: Option<String>, pending: bool, limit: i64) -> Result<Vec<Value>, ApiError> {
    let now = util::now_ms();
    let mut v = ctx.db.call(move |c| {
        let mut q = c.prepare(&format!("SELECT {COLS} FROM api_steps WHERE (?1 IS NULL OR service=?1) AND (?2=0 OR (status='pending' AND expires>=?3)) ORDER BY at DESC LIMIT ?4"))?;
        let r = q.query_map(params![service, pending as i64, now, limit], row)?.collect::<Result<Vec<_>, _>>();
        r
    }).await?;
    for s in v.iter_mut() {
        if s["status"] == "pending" && s["expires"].as_i64().unwrap_or(0) < now {
            s["status"] = json!("expired");
        }
    }
    Ok(v)
}

/// The people a page offers (its person field's table), or the plugin's people.
pub fn people(p: &Plugin, page: Option<&Page>) -> Vec<Value> {
    let table = page.and_then(|pg| pg.fields.iter().find_map(|f| if let Field::Person(pf) = f { Some(pf.from.clone()) } else { None })).unwrap_or("people".into());
    p.data.get(&table).and_then(|t| t.as_array()).cloned().unwrap_or_default()
}

pub fn find_person(p: &Plugin, key: &str) -> Value {
    let d = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    p.data.values().filter_map(|t| t.as_array()).flatten()
        .find(|r| r.get("cpf").map(|c| !d(key).is_empty() && d(&expr::text(c)) == d(key)).unwrap_or(false) || r.get("name").map(|n| expr::text(n).eq_ignore_ascii_case(key)).unwrap_or(false))
        .cloned().unwrap_or(Value::Null)
}

fn scope(req: &Req, p: &Plugin) -> Map<String, Value> {
    let mut sc = Map::new();
    sc.insert("query".into(), Value::Object(req.query.clone()));
    sc.insert("body".into(), req.body.clone());
    sc.insert("headers".into(), json!(req.headers));
    sc.insert("data".into(), json!(p.data));
    sc.insert("caller".into(), json!(req.caller));
    sc
}

fn html_out(status: u16, body: String) -> Outcome {
    Outcome { status, headers: vec![("content-type".into(), "text/html; charset=utf-8".into()), ("cache-control".into(), "no-store".into())], body: body.into_bytes(), ..Default::default() }
}

fn redirect(to: &str) -> Outcome {
    Outcome { status: 302, headers: vec![("location".into(), to.into())], ..Default::default() }
}

fn with_params(uri: &str, kv: &[(&str, &str)]) -> String {
    let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
    let q = kv.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("{k}={}", enc(v))).collect::<Vec<_>>().join("&");
    format!("{uri}{}{q}", if uri.contains('?') { "&" } else { "?" })
}

/// A page route was called: a push (answer at once) or the page itself.
pub async fn open(ctx: &Ctx, p: &Plugin, idx: usize, req: &Req, base: &str) -> Outcome {
    let pg = &p.pages[idx];
    let mut sc = scope(req, p);
    let q = |k: &str| req.query.get(k).map(expr::text).unwrap_or_default();
    if let Some(pu) = &pg.push {
        if expr::test(&pu.when, &sc).unwrap_or(false) {
            let cpf = expr::eval(&pu.person, &sc).map(|v| expr::text(&v)).unwrap_or_default();
            let person = find_person(p, &cpf);
            let id = match create(ctx, &p.service.name, idx, "push", Value::Object(req.query.clone()), &cpf, pu.seconds as i64 * 1000).await {
                Ok(i) => i,
                Err(e) => return html_out(500, e.message),
            };
            sc.insert("step".into(), json!({"code": id, "id": id}));
            let Some(a) = p.answers.iter().find(|a| a.name == pu.answer) else { return html_out(500, "push.answer missing".into()) };
            let (status, headers, body, answered) = render_answer(a, &sc);
            let who = if person.is_null() { expr::mask(&cpf) } else { expr::text(&person["name"]) };
            return Outcome { status, headers, body, answered, answer: a.name.clone(), sentence: format!("Push approval sent to {who}'s phone; the app polls"), steps: vec![json!({"step": "page", "text": format!("Push approval {id} for {who}, {} s", pu.seconds), "ok": true})], ..Default::default() };
        }
    }
    let mut missing = vec![];
    if pg.checks.iter().any(|c| c == "redirect_uri") && q("redirect_uri").is_empty() {
        missing.push("redirect_uri");
    }
    if pg.checks.iter().any(|c| c == "state") && q("state").is_empty() {
        missing.push("state");
    }
    if pg.checks.iter().any(|c| c == "pkce_s256") && (q("code_challenge").is_empty() || !q("code_challenge_method").eq_ignore_ascii_case("S256")) {
        missing.push("code_challenge (S256)");
    }
    if !missing.is_empty() {
        let msg = format!("The app opened the page without {}.", missing.join(", "));
        let mut o = html_out(400, page_html(p, pg, None, base, &sc, Some(&msg), true));
        o.answer = "invalid_request".into();
        o.sentence = format!("{} page refused: {msg}", pg.title);
        return o;
    }
    let id = match create(ctx, &p.service.name, idx, "page", Value::Object(req.query.clone()), "", 10 * 60_000).await {
        Ok(i) => i,
        Err(e) => return html_out(500, e.message),
    };
    let step = get_raw(ctx, &id).await.ok().flatten();
    let mut o = html_out(200, page_html(p, pg, step.as_ref(), base, &sc, None, false));
    o.answer = "page".into();
    o.sentence = format!("{} page opened for {}", pg.title, if q("client_id").is_empty() { req.caller.clone() } else { q("client_id") });
    o.steps = vec![json!({"step": "page", "text": format!("Step {id} waits for a person"), "ok": true})];
    o
}

/// The page itself, or the push page "on the phone" (GET /__stuntman/steps/{id}).
pub async fn show(ctx: &Ctx, p: &Plugin, id: &str, base: &str) -> Outcome {
    let Ok(Some(s)) = get(ctx, id).await else { return html_out(404, shell("Not found", "<p class=err>This sign-in link was already used or never existed. Start again from the app.</p>")) };
    let idx = s["page"].as_u64().unwrap_or(0) as usize;
    let Some(pg) = p.pages.get(idx) else { return html_out(404, shell("Not found", "<p class=err>The plugin no longer has this page.</p>")) };
    let mut sc = Map::new();
    sc.insert("query".into(), s["params"].clone());
    sc.insert("data".into(), json!(p.data));
    html_out(200, page_html(p, pg, Some(&s), base, &sc, None, false))
}

/// What a person (or an agent) did on a step: authorize | deny | approve | reject | timeout.
pub async fn act(ctx: &Ctx, p: &Plugin, id: &str, form: &Map<String, Value>, by: &str, base: &str) -> Result<Outcome, ApiError> {
    let s = get(ctx, id).await?.ok_or_else(|| ApiError::not_found(format!("No step {id}")))?;
    let f = |k: &str| form.get(k).map(expr::text).unwrap_or_default();
    let action = f("action").to_lowercase();
    let idx = s["page"].as_u64().unwrap_or(0) as usize;
    let pg = p.pages.get(idx).ok_or_else(|| ApiError::not_found("The plugin no longer has this page"))?;
    let title = pg.title.clone();
    if s["status"] != "pending" {
        return Err(ApiError::new(409, format!("This step is already {}", s["status"].as_str().unwrap_or(""))));
    }
    let params = s["params"].clone();
    let pq = |k: &str| params.get(k).map(expr::text).unwrap_or_default();
    if s["kind"] == "push" {
        let (st, grant, text) = match action.as_str() {
            "approve" | "authorize" => ("approved", Some(util::id("grt")), "approved"),
            "reject" | "deny" => ("rejected", None, "rejected"),
            "timeout" => ("expired", None, "let it time out"),
            _ => return Err(ApiError::bad("action: approve | reject | timeout")),
        };
        update(ctx, id, st, None, grant, by, &format!("{} {text}", who_said(by))).await?;
        let mut o = redirect(&format!("{base}/__stuntman/steps/{id}"));
        o.answer = st.into();
        o.sentence = format!("{title}: push {text} by {}", who_said(by));
        return Ok(o);
    }
    let uri = pq("redirect_uri");
    let state = pq("state");
    if ["deny", "reject"].contains(&action.as_str()) || pg.buttons.get(1).map(|b| b.eq_ignore_ascii_case(&action)).unwrap_or(false) {
        update(ctx, id, "denied", None, None, by, &format!("{} clicked {}", who_said(by), pg.buttons.get(1).cloned().unwrap_or("Deny".into()))).await?;
        let mut o = redirect(&with_params(&uri, &[("error", "access_denied"), ("state", &state)]));
        o.answer = "access_denied".into();
        o.sentence = format!("{title}: denied by {}", who_said(by));
        return Ok(o);
    }
    // Authorize: who, and the code.
    let person = find_person(p, &f("person"));
    if person.is_null() {
        return Err(ApiError::bad("Pick who is signing"));
    }
    let name = expr::text(&person["name"]);
    for fl in &pg.fields {
        if let Field::Otp(o) = fl {
            let code: String = f("otp").chars().filter(|c| !c.is_whitespace()).collect();
            if code.len() != o.digits as usize || !code.chars().all(|c| c.is_ascii_digit()) || code == o.refuse {
                let tries = s["tries"].as_i64().unwrap_or(0) + 1;
                if tries >= o.tries as i64 {
                    update(ctx, id, "locked", None, None, by, &format!("wrong code {tries}×: locked")).await?;
                    return Err(ApiError::new(423, format!("Wrong code {tries} times: the page is locked, as the provider does. Start again from the app.")));
                }
                note(ctx, id, &format!("wrong code ({code}), try {tries} of {}", o.tries), Some(tries)).await?;
                return Err(ApiError::new(422, format!("Invalid code. {} tries left.", o.tries as i64 - tries)));
            }
        }
    }
    let grant = util::id("grt");
    update(ctx, id, "authorized", Some(expr::text(&person["cpf"])), Some(grant.clone()), by, &format!("{} picked {name}, code ok", who_said(by))).await?;
    let mut o = redirect(&with_params(&uri, &[("code", &grant), ("state", &state)]));
    o.answer = "authorized".into();
    o.sentence = format!("{title}: authorized as {name} by {}", who_said(by));
    Ok(o)
}

fn who_said(by: &str) -> String {
    match by {
        "" | "browser" => "the person".into(),
        b => b.trim_start_matches("mcp:").trim_start_matches("api:").to_string(),
    }
}

fn token_answer(ctx_tok: &str, ttl_s: i64, person: &Value) -> Outcome {
    let body = json!({"access_token": ctx_tok, "token_type": "Bearer", "expires_in": ttl_s, "scope": "signature_session",
        "authorized_identification": expr::text(&person["cpf"]).chars().filter(|c| c.is_ascii_digit()).collect::<String>(), "authorized_identification_type": "CPF"});
    Outcome { status: 200, headers: vec![("content-type".into(), "application/json".into())], body: serde_json::to_vec(&body).unwrap_or_default(), answer: "token".into(), answered: json!({"expires_in": ttl_s}), ..Default::default() }
}

fn grant_refused(why: &str, error: &str) -> Outcome {
    Outcome { status: 400, headers: vec![("content-type".into(), "application/json".into())], body: serde_json::to_vec(&json!({"error": error, "error_description": why})).unwrap_or_default(),
        answer: error.into(), sentence: format!("Token refused: {why}"), steps: vec![json!({"step": "receive", "text": why, "ok": false})], ..Default::default() }
}

/// A `grant:` route: authorization_code (from a page or a push) or password (CPF + OTP).
pub async fn grant(ctx: &Ctx, p: &Plugin, r: &Route, req: &Req, dry: bool) -> Outcome {
    let b = |k: &str| req.body.get(k).or(req.query.get(k)).map(expr::text).unwrap_or_default();
    let lifetime = b("lifetime").parse::<i64>().ok().filter(|x| *x > 0).unwrap_or(3600);
    let (person, how) = match r.grant.as_str() {
        "password" => {
            let person = find_person(p, &b("username"));
            if person.is_null() {
                return grant_refused("no person with this CPF", "invalid_grant");
            }
            let otp = b("password");
            let refuse = p.pages.iter().flat_map(|pg| pg.fields.iter()).find_map(|f| if let Field::Otp(o) = f { Some((o.digits as usize, o.refuse.clone())) } else { None }).unwrap_or((6, "000000".into()));
            if otp.len() != refuse.0 || !otp.chars().all(|c| c.is_ascii_digit()) || otp == refuse.1 {
                return grant_refused("invalid OTP", "invalid_grant");
            }
            (person, "CPF and OTP".to_string())
        }
        _ => {
            let code = b("code");
            let gc = code.clone();
            let svc = p.service.name.clone();
            let step = match ctx.db.call(move |c| c.query_row(&format!("SELECT {COLS} FROM api_steps WHERE grant_code=?1 AND service=?2"), params![gc, svc], row).optional()).await {
                Ok(Some(s)) => s,
                _ => return grant_refused("unknown code", "invalid_grant"),
            };
            if step["used"] == true {
                return grant_refused("the code was already exchanged", "invalid_grant");
            }
            if step["done_at"].as_i64().unwrap_or(0) + 10 * 60_000 < util::now_ms() {
                return grant_refused("the code expired (10 min)", "invalid_grant");
            }
            let params = &step["params"];
            let challenge = params.get("code_challenge").map(expr::text).unwrap_or_default();
            if !challenge.is_empty() {
                let verifier = b("code_verifier");
                if B64U.encode(sha2::Sha256::digest(verifier.as_bytes())) != challenge {
                    return grant_refused(if verifier.is_empty() { "code_verifier missing (PKCE)" } else { "code_verifier does not match the challenge (PKCE)" }, "invalid_grant");
                }
            }
            let sent_uri = b("redirect_uri");
            let want_uri = params.get("redirect_uri").map(expr::text).unwrap_or_default();
            if step["kind"] == "page" && !sent_uri.is_empty() && sent_uri != want_uri {
                return grant_refused("redirect_uri differs from the authorize request", "invalid_grant");
            }
            if !dry {
                let id = step["id"].as_str().unwrap_or("").to_string();
                let _ = ctx.db.call(move |c| c.execute("UPDATE api_steps SET used=1 WHERE id=?1", [&id]).map(|_| ())).await;
                let _ = note(ctx, step["id"].as_str().unwrap_or(""), "code exchanged for a token", None).await;
            }
            (find_person(p, step["person"].as_str().unwrap_or("")), format!("code from {}", if step["kind"] == "push" { "a push approval" } else { "the page" }))
        }
    };
    let cpf: String = expr::text(&person["cpf"]).chars().filter(|c| c.is_ascii_digit()).collect();
    let tok = if dry { "dbl_try_it".to_string() } else { store::issue_token(ctx, &p.service.name, &format!("person:{cpf}"), lifetime * 1000).await.unwrap_or_default() };
    let mut o = token_answer(&tok, lifetime, &person);
    o.sentence = format!("Token for {} ({how}), {lifetime} s", expr::text(&person["name"]));
    o.steps = vec![json!({"step": "receive", "text": format!("{} for {}", how, expr::text(&person["name"])), "ok": true})];
    o
}

// ---- HTML ----

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn shell(title: &str, inner: &str) -> String {
    format!(r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t}</title><style>
:root{{--ink:#16181B;--t2:#55575A;--cap:#8E8E88;--rule:#E3E3DF;--soft:#F3F2EE;--line:#C9C9C4;--warn:#8A5A00;--err:#B42318;--ok:#1F7A3F;--link:#1F5FBF}}
*{{box-sizing:border-box}}body{{margin:0;background:#FAFAF8;color:var(--ink);font:14px/1.45 Archivo,-apple-system,"Segoe UI",sans-serif}}
.wrap{{max-width:560px;margin:48px auto;padding:0 16px}}.card{{background:#fff;border:1px solid var(--rule);border-radius:12px;padding:28px}}
.brand{{display:flex;gap:12px;align-items:center}}.bar{{width:24px;height:5px;border-radius:3px;background:var(--link)}}.brand b{{font-size:17px}}
.sub{{color:var(--cap);font-size:12px;margin:2px 0 0 36px}}h1{{font-size:18px;font-weight:500;margin:24px 0 18px}}
.cap{{font-size:10px;font-weight:600;letter-spacing:1.2px;color:var(--cap);text-transform:uppercase;margin:18px 0 8px}}
label.p{{display:flex;gap:12px;align-items:flex-start;border:1px solid var(--rule);border-radius:8px;padding:12px 14px;margin:0 0 8px;cursor:pointer}}
label.p:has(input:checked){{border-color:var(--ink);background:var(--soft)}}label.p small{{display:block;color:var(--cap);font-size:12px}}
.warn{{color:var(--warn)}}.err{{color:var(--err)}}input[type=text],select{{font:15px "JetBrains Mono",ui-monospace,monospace;border:1px solid var(--line);border-radius:6px;padding:9px 12px;width:200px}}
.hint{{color:var(--cap);font-size:12px}}.row{{display:flex;gap:10px;justify-content:flex-end;margin-top:24px}}
button{{font:500 14px Archivo,sans-serif;border-radius:6px;padding:9px 16px;border:1px solid var(--line);background:#fff;cursor:pointer}}button.primary{{background:var(--ink);color:#fff;border-color:var(--ink)}}
.foot{{color:var(--cap);font-size:12px;margin-top:20px}}.log{{margin-top:22px;border-top:1px solid var(--rule);padding-top:14px;font-size:12px;color:var(--t2)}}.log span{{font-family:"JetBrains Mono",monospace;color:var(--cap);margin-right:10px}}
.bad{{background:#FDECEA;color:var(--err);border-radius:8px;padding:10px 14px;margin-bottom:14px}}.bar2{{height:6px;background:var(--soft);border-radius:3px;margin:6px 0}}.bar2 i{{display:block;height:6px;background:var(--ink);border-radius:3px}}
</style></head><body><div class="wrap">{inner}</div><script>document.querySelectorAll('[data-at]').forEach(e=>e.textContent=new Date(+e.dataset.at).toLocaleTimeString([],{{hour12:false}}))</script></body></html>"#, t = esc(title))
}

fn hhmmss(ms: i64) -> String {
    let s = (ms / 1000).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// The declared page drawn by Stuntman: person choice, code, buttons; or the push page.
pub fn page_html(p: &Plugin, pg: &Page, step: Option<&Value>, base: &str, sc: &Map<String, Value>, error: Option<&str>, preview: bool) -> String {
    let ask = expr::render(&pg.ask, sc).unwrap_or_else(|_| pg.ask.clone());
    let mut h = format!(r#"<div class="card"><div class="brand"><span class="bar"></span><b>{}</b></div><div class="sub">stand-in by Stuntman · {}</div>"#, esc(&pg.title), esc(&p.service.name));
    if let (Some(e), None) = (error, step) {
        h += &format!(r#"<h1>{}</h1><div class="bad">{}</div><p class="hint">Real providers refuse this too; fix the authorize request in the app.</p></div>"#, esc(&ask), esc(e));
        return shell(&pg.title, &h);
    }
    let status = step.map(|s| s["status"].as_str().unwrap_or("").to_string()).unwrap_or("pending".into());
    let id = step.map(|s| s["id"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let push = step.map(|s| s["kind"] == "push").unwrap_or(false);
    if status != "pending" {
        let msg = match status.as_str() {
            "authorized" | "approved" => "Done: the app got its code.",
            "denied" | "rejected" => "Denied: the app was told so.",
            "locked" => "Locked after too many wrong codes. Start again from the app.",
            "expired" => "Nobody answered in time; the app sees it as expired.",
            _ => "This step is closed.",
        };
        h += &format!(r#"<h1>{}</h1><p class="{}">{msg}</p>"#, esc(&ask), if ["authorized", "approved"].contains(&status.as_str()) { "" } else { "warn" });
    } else if push {
        let s = step.unwrap();
        let person = find_person(p, s["person"].as_str().unwrap_or(""));
        let left = (s["expires"].as_i64().unwrap_or(0) - util::now_ms()).max(0) / 1000;
        let total = pg.push.as_ref().map(|x| x.seconds as i64).unwrap_or(180).max(1);
        h += &format!(r#"<h1>{} asks to sign with your certificate</h1><div class="cap">Certificate</div><label class="p" style="cursor:default;background:var(--soft)"><span><b>{}</b><small>CPF {} · {}</small></span></label>
<div class="cap">Expires</div><div class="bar2"><i style="width:{}%"></i></div><div class="hint">{}:{:02} left</div>
<form method="post" action="{base}/__stuntman/steps/{id}"><div class="row" style="justify-content:flex-start"><button class="primary" name="action" value="approve">Approve</button><button name="action" value="reject">Reject</button><button name="action" value="timeout">Let it time out</button></div></form>
<p class="foot">Approve: the app's next poll gets its token. Reject or time out: the polls keep answering without one.</p>"#,
            esc(s["params"].get("client_id").map(expr::text).unwrap_or("The app".into()).as_str()), esc(&expr::text(&person["name"])), esc(&expr::mask(&expr::text(&person["cpf"]))), cert_label(&person), left * 100 / total, left / 60, left % 60);
    } else {
        h += &format!("<h1>{}</h1>", esc(&ask));
        if let Some(e) = error {
            h += &format!(r#"<div class="bad">{}</div>"#, esc(e));
        }
        h += &format!(r#"<form method="post" action="{base}/__stuntman/steps/{id}">"#);
        for f in &pg.fields {
            match f {
                Field::Person(pf) => {
                    h += &format!(r#"<div class="cap">{}</div>"#, esc(if pf.label.is_empty() { "Who is signing" } else { &pf.label }));
                    for (i, row) in people(p, Some(pg)).iter().enumerate() {
                        let cls = match row["certificate"].as_str().unwrap_or("valid") { "expired" => "warn", "none" => "hint", _ => "" };
                        h += &format!(r#"<label class="p"><input type="radio" name="person" value="{}" {}><span class="{cls}"><b style="font-weight:500">{}</b><small>CPF {} · {}</small></span></label>"#,
                            esc(&expr::text(&row["cpf"])), if i == 0 { "checked" } else { "" }, esc(&expr::text(&row["name"])), esc(&expr::mask(&expr::text(&row["cpf"]))), cert_label(row));
                    }
                }
                Field::Otp(o) => {
                    h += &format!(r#"<div class="cap">{}</div><input type="text" name="otp" inputmode="numeric" autocomplete="one-time-code" maxlength="{}" placeholder="{}"> <span class="hint">Any {} digits work; {} is refused on purpose.</span>"#,
                        esc(if o.label.is_empty() { "Code" } else { &o.label }), o.digits, "0".repeat(o.digits as usize), o.digits, esc(&o.refuse));
                }
                Field::Text(t) => h += &format!(r#"<div class="cap">{}</div><input type="text" name="{}">"#, esc(if t.label.is_empty() { &t.name } else { &t.label }), esc(&t.name)),
                Field::Choice(c) => {
                    h += &format!(r#"<div class="cap">{}</div><select name="{}">{}</select>"#, esc(if c.label.is_empty() { &c.name } else { &c.label }), esc(&c.name), c.options.iter().map(|o| format!("<option>{}</option>", esc(o))).collect::<String>());
                }
            }
        }
        let a = pg.buttons.first().cloned().unwrap_or("Authorize".into());
        let d = pg.buttons.get(1).cloned();
        h += &format!(r#"<div class="row">{}<button class="primary" name="action" value="authorize" {}>{}</button></div></form>"#,
            d.map(|d| format!(r#"<button name="action" value="deny" {}>{}</button>"#, if preview { "disabled" } else { "" }, esc(&d))).unwrap_or_default(), if preview { "disabled" } else { "" }, esc(&a));
        h += r#"<p class="foot">Authorize sends the browser back to the app with a code; the token call then checks the PKCE verifier, as the real provider does.</p>"#;
    }
    if let Some(s) = step {
        if let Some(log) = s["log"].as_array() {
            h += r#"<div class="log"><div class="cap" style="margin-top:0">What Stuntman records</div>"#;
            for l in log {
                h += &format!(r#"<div><span data-at="{}">{}</span>{}</div>"#, l["at"].as_i64().unwrap_or(0), hhmmss(l["at"].as_i64().unwrap_or(0)), esc(&expr::text(&l["text"])));
            }
            h += "</div>";
        }
    }
    if preview {
        h += r#"<p class="foot">Preview: nothing is recorded.</p>"#;
    }
    h += "</div>";
    shell(&pg.title, &h)
}

fn cert_label(row: &Value) -> String {
    match row["certificate"].as_str().unwrap_or("valid") {
        "expired" => "certificate expired".into(),
        "none" => "no certificate".into(),
        _ => format!("{}, valid", row.get("kind").map(expr::text).unwrap_or("e-CPF A3".into())),
    }
}
