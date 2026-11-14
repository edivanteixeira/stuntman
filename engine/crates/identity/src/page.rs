//! I2 · The sign-in page Stuntman serves in place of the provider's: pick a synthetic person (each one
//! a ready-made situation), or type claims once; then the MFA and consent steps; errors as the provider
//! shows them; and the form that posts the answer back to the app (form_post).

use serde_json::Value;

use crate::config::{self, Provider, User};

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

const CSS: &str = r#"
:root{--surface:#fff;--ground:#fafaf8;--subtle:#f1f1ee;--soft:#f6f6f3;--rule:#e3e3df;--border:#c9c9c4;--caption:#6e6e68;--text-2:#55575a;--ink:#16181b;--link:#005a9e;--attention:#8a5a00;--danger:#b3261e;--blue:#0070c9}
*{box-sizing:border-box}body{margin:0;background:var(--ground);color:var(--ink);font:400 13px/1.45 Archivo,system-ui,sans-serif}
.served{max-width:1120px;margin:20px auto 0;padding:0 16px;color:var(--caption);font-size:12px}
.wrap{max-width:1120px;margin:24px auto 60px;padding:0 16px;display:flex;gap:40px;align-items:flex-start;justify-content:center}
.card{background:var(--surface);border:1px solid var(--rule);border-radius:10px;padding:26px 28px;width:560px;max-width:100%}
.brand{display:flex;align-items:center;gap:12px}.brand i{display:block;width:22px;height:4px;border-radius:2px;background:var(--blue)}
h1{font:600 18px/1.2 Archivo,sans-serif;margin:0}.sub{color:var(--caption);font-size:12px;margin:4px 0 0 34px}
p{margin:18px 0 10px}.mono{font-family:"JetBrains Mono",ui-monospace,monospace}
.u{display:flex;align-items:center;gap:14px;border:1px solid var(--rule);border-radius:6px;padding:10px 14px;margin:8px 0;cursor:pointer}
.u:has(input:checked){border:1.5px solid var(--ink);background:var(--soft)}.u input{display:none}
.av{width:32px;height:32px;border-radius:50%;background:var(--subtle);display:flex;align-items:center;justify-content:center;font:600 12px Archivo,sans-serif;color:var(--text-2);flex:none}
.u .who{flex:1;min-width:0}.u .em{font:400 11px "JetBrains Mono",monospace;color:var(--caption)}.u .tags{font-size:12px;color:var(--caption);text-align:right;max-width:210px}.u .tags .w{color:var(--attention)}
.actions{display:flex;justify-content:flex-end;gap:10px;margin-top:18px}
button,.btn{font:500 13px Archivo,sans-serif;border:1px solid var(--border);background:#fff;border-radius:6px;padding:7px 16px;cursor:pointer;color:var(--ink);text-decoration:none}
button.primary{background:var(--ink);border-color:var(--ink);color:#fff}
.note{color:var(--caption);font-size:12px;margin-top:22px}
details{margin-top:12px}summary{color:var(--link);cursor:pointer;font-weight:500}
input[type=text]{width:100%;border:1px solid var(--border);border-radius:6px;padding:8px 10px;font:inherit;margin-top:4px}
label.f{display:block;margin-top:10px;font-size:12px;color:var(--text-2)}
.code{font:500 22px "JetBrains Mono",monospace;letter-spacing:6px;text-align:center}
.err{color:var(--danger);font-weight:500}.side{width:300px;padding-top:24px}
.side h3{font:600 11px Archivo,sans-serif;letter-spacing:1px;color:var(--caption);margin:0 0 10px}
.side table{border-collapse:collapse;font-size:12px}.side td{padding:3px 12px 3px 0;vertical-align:top}.side td:first-child{color:var(--text-2)}
.scopes li{margin:4px 0}
@media(max-width:900px){.wrap{flex-direction:column;align-items:stretch}.side{width:auto}}
"#;

fn shell(title: &str, body: &str, side: &str) -> String {
    format!(r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t}</title>
<link href="https://fonts.googleapis.com/css2?family=Archivo:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap" rel="stylesheet"><style>{CSS}</style></head>
<body><div class="served">A page served by Stuntman in place of the identity provider's sign-in</div><div class="wrap"><div class="card">{body}</div>{side}</div></body></html>"#, t = esc(title))
}

fn head(p: &Provider, title: &str) -> String {
    format!(r#"<div class="brand"><i></i><h1>{}</h1></div><div class="sub">{} stand-in by Stuntman  ·  tenant {}</div>"#, esc(title), config::flavour_label(&p.flavour), esc(&p.tenant))
}

fn initials(n: &str) -> String {
    n.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

/// What makes this person a situation, as the page shows it.
fn tags(p: &Provider, u: &User, client_id: &str) -> String {
    let mut plain: Vec<String> = u.groups.iter().take(2).cloned().collect();
    if u.groups.len() > 2 { plain.push(format!("{} groups", u.groups.len())); }
    let mut warn = vec![];
    if u.mfa { plain.push("MFA on".into()); }
    if !u.enabled { warn.push("account disabled (AADSTS50057)".to_string()); }
    if u.tenant != p.tenant && !u.guest { warn.push(format!("other tenant: {}", u.tenant)); }
    if u.guest { warn.push("guest, personal account".into()); }
    if u.phone_lost { warn.push("phone lost".into()); }
    if u.enabled && !u.consented.iter().any(|c| c == client_id) && u.tenant == p.tenant { warn.push("never consented".into()); }
    let mut out = esc(&plain.join("  ·  "));
    if !warn.is_empty() { out += &format!("{}<span class=\"w\">{}</span>", if plain.is_empty() { "" } else { "  ·  " }, esc(&warn.join("  ·  "))); }
    out
}

fn side(d: &Value) -> String {
    let g = |k: &str| d[k].as_str().unwrap_or("").to_string();
    let (prompt, mode) = (g("prompt"), g("response_mode"));
    let (prompt, mode) = (if prompt.is_empty() { "—".to_string() } else { prompt }, if mode.is_empty() { "query".to_string() } else { mode });
    let pkce = if g("code_challenge").is_empty() { "none".to_string() } else { format!("{} ✓", if g("code_challenge_method").is_empty() { "plain".into() } else { g("code_challenge_method") }) };
    format!(r#"<div class="side"><h3>WHAT THE APP ASKED FOR</h3><table>
<tr><td>client</td><td class="mono">{}</td></tr><tr><td>redirect</td><td class="mono">{}</td></tr><tr><td>scope</td><td class="mono">{}</td></tr>
<tr><td>prompt</td><td class="mono">{}</td></tr><tr><td>PKCE</td><td class="mono">{}</td></tr><tr><td>state</td><td class="mono">{}</td></tr><tr><td>answer</td><td class="mono">{} · {}</td></tr></table>
<div class="note">Agents skip the page: sign_in_as("Emma Carter") follows the same steps through MCP.</div></div>"#,
        esc(&g("client_id")), esc(&g("redirect_uri")), esc(&g("scope")), esc(&prompt), esc(&pkce),
        if g("state").is_empty() { "none" } else { "✓ will be echoed" }, esc(&g("response_type")), esc(&mode))
}

pub fn pick(p: &Provider, pid: &str, d: &Value, env: &str, action: &str) -> String {
    let cid = d["client_id"].as_str().unwrap_or("");
    let app = p.app(cid).map(|a| a.name.clone()).unwrap_or(cid.to_string());
    let hint = d["login_hint"].as_str().unwrap_or("").to_lowercase();
    let first = p.users.iter().position(|u| !hint.is_empty() && u.email.to_lowercase() == hint).unwrap_or(0);
    let rows: String = p.users.iter().enumerate().map(|(i, u)| format!(
        r#"<label class="u"><input type="radio" name="user" value="{id}"{chk}><span class="av">{ini}</span><span class="who"><div>{name}</div><div class="em">{em}</div></span><span class="tags">{tags}</span></label>"#,
        id = esc(&u.id), chk = if i == first { " checked" } else { "" }, ini = esc(&initials(&u.name)), name = esc(&u.name), em = esc(&u.email), tags = tags(p, u, cid))).collect();
    let body = format!(r#"{head}<form method="post" action="{action}"><input type="hidden" name="pending" value="{pid}">
<p>{app} ({env}) wants you to sign in. Pick who is signing in:</p>{rows}
<details><summary>+ Sign in with other claims…</summary><div class="note" style="margin-top:6px">Email, name, groups, roles and tenant typed by hand for one sign-in; nothing is saved as a user.</div>
<label class="f">Email<input type="text" name="c_email" placeholder="test.user@contoso.example.com"></label><label class="f">Name<input type="text" name="c_name" placeholder="Test User"></label>
<label class="f">Groups (comma between)<input type="text" name="c_groups" placeholder="Finance, Signers"></label><label class="f">Roles<input type="text" name="c_roles" placeholder="Approver"></label>
<label class="f">Tenant<input type="text" name="c_tenant" placeholder="{tenant}"></label></details>
<div class="actions"><button name="do" value="cancel">Cancel</button><button class="primary" name="do" value="pick">Continue</button></div></form>
<div class="note">Continue goes to the MFA step if the user has it, then consent if the app needs it, then back to {app} with a code. Cancel returns error=access_denied.</div>"#,
        head = head(p, "Sign in"), app = esc(&app), env = esc(&env.to_uppercase()), tenant = esc(&p.tenant));
    shell("Sign in", &body, &side(d))
}

pub fn mfa(p: &Provider, pid: &str, d: &Value, action: &str) -> String {
    let tries = d["mfa_tries"].as_i64().unwrap_or(0);
    let user = p.user(d["user"].as_str().unwrap_or("")).map(|u| u.name.clone()).unwrap_or_default();
    let body = format!(r#"{head}<form method="post" action="{action}"><input type="hidden" name="pending" value="{pid}">
<p>Enter the code from your authenticator{who}.</p><input type="text" class="code" name="code" inputmode="numeric" maxlength="6" autofocus autocomplete="one-time-code" placeholder="••••••">
{wrong}<div class="actions"><button name="do" value="cancel">Cancel</button><button class="primary" name="do" value="mfa">Verify</button></div></form>
<div class="note">Any 6 digits pass; 000000 fails. Three failures lock the user for 5 min.</div>"#,
        head = head(p, "Verify your identity"), who = if user.is_empty() { String::new() } else { format!(", {}", esc(&user)) },
        wrong = if tries > 0 { format!(r#"<p class="err">That code didn't work ({tries} of 3).</p>"#) } else { String::new() });
    shell("Verify your identity", &body, &side(d))
}

fn scope_sentence(s: &str) -> String {
    match s { "openid" => "Sign you in".into(), "profile" => "Read your profile".into(), "email" => "View your email address".into(), "offline_access" => "Keep access to what you allowed (offline)".into(), x => format!("Access {x}") }
}

pub fn consent(p: &Provider, pid: &str, d: &Value, action: &str) -> String {
    let cid = d["client_id"].as_str().unwrap_or("");
    let app = p.app(cid).map(|a| a.name.clone()).unwrap_or(cid.to_string());
    let items: String = d["scope"].as_str().unwrap_or("").split_whitespace().map(|s| format!("<li>{}</li>", esc(&scope_sentence(s)))).collect();
    let body = format!(r#"{head}<form method="post" action="{action}"><input type="hidden" name="pending" value="{pid}">
<p><b>{app}</b> wants to:</p><ul class="scopes">{items}</ul>
<div class="actions"><button name="do" value="cancel">Cancel</button><button class="primary" name="do" value="consent">Accept</button></div></form>
<div class="note">Accepting is remembered per user and app until revoked in Users.</div>"#, head = head(p, "Permissions requested"), app = esc(&app));
    shell("Permissions requested", &body, &side(d))
}

pub fn admin(p: &Provider, pid: &str, d: &Value, action: &str) -> String {
    let cid = d["client_id"].as_str().unwrap_or("");
    let app = p.app(cid).map(|a| a.name.clone()).unwrap_or(cid.to_string());
    let scopes: String = d["admin_only"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).map(|x| format!("<li class=\"mono\">{}</li>", esc(x))).collect()).unwrap_or_default();
    let body = format!(r#"{head}<form method="post" action="{action}"><input type="hidden" name="pending" value="{pid}">
<p><b>Need admin approval.</b> {app} needs permission to access resources in your organization that only an admin can grant:</p><ul class="scopes">{scopes}</ul>
<div class="actions"><button name="do" value="cancel">Back to the app</button></div></form>
<div class="note">An administrator grants it in Stuntman (Apps › Grant admin consent, or grant_admin_consent); then sign in again.</div>"#, head = head(p, "Approval required"), app = esc(&app));
    shell("Approval required", &body, &side(d))
}

pub fn error(p: Option<&Provider>, code: &str, message: &str, back: Option<&str>) -> String {
    let h = match p { Some(p) => head(p, "Sorry, but we're having trouble signing you in."), None => r#"<div class="brand"><i></i><h1>Sorry, but we're having trouble signing you in.</h1></div>"#.to_string() };
    let body = format!(r#"{h}<p class="err">{code}</p><p>{msg}</p>{back}<div class="note">This is the provider's own error page, as the real one shows it.</div>"#,
        code = esc(code), msg = esc(message), back = back.map(|b| format!(r#"<div class="actions"><a class="btn" href="{}">Back to the app</a></div>"#, esc(b))).unwrap_or_default());
    shell("Sign in error", &body, "")
}

/// response_mode=form_post: the browser posts the answer to the app's redirect URI.
pub fn form_post(url: &str, fields: &[(String, String)]) -> String {
    let inputs: String = fields.iter().map(|(k, v)| format!(r#"<input type="hidden" name="{}" value="{}">"#, esc(k), esc(v))).collect();
    format!(r#"<!doctype html><html><head><meta charset="utf-8"><title>Working…</title></head><body onload="document.forms[0].submit()">
<form method="post" action="{}">{inputs}<noscript><button>Continue to the app</button></noscript></form></body></html>"#, esc(url))
}
