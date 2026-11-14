//! I4 · Apps: registrations — who may ask for tokens, where they come back (exact match), the secret
//! (from Secrets), permissions and admin consent, optional claims; the tokens each app gets, the
//! Authority to set, and the last errors the app caused.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{code_note, fetch, href, sel, Head};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Dialog, Toggle},
};

fn list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn kind_label(k: &str) -> &'static str {
    match k { "spa" => "single-page app · public (PKCE)", "daemon" => "daemon · client credentials", "api" => "exposes an API", _ => "web sign-in · confidential" }
}

#[component]
pub fn Apps() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let qm = use_query_map();
    let p = sel();
    let data = fetch("apps", p);
    let tenant = fetch("tenant", p);
    let item = Memo::new(move |_| qm.get().get("app").unwrap_or_default());
    let cur = Memo::new(move |_| { let a = data.get()["apps"].as_array().cloned().unwrap_or_default(); let i = item.get(); a.iter().find(|x| s(x, "client_id") == i).cloned().or_else(|| a.first().cloned()).unwrap_or(Value::Null) });
    let draft = RwSignal::new(Value::Null);
    Effect::new(move |_| draft.set(cur.get()));
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != cur.get());
    let errors = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        let (prov, cid) = (p.get(), s(&cur.get(), "client_id"));
        if prov.is_empty() || cid.is_empty() { errors.set(vec![]); return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/identity/sign-ins?provider={}&result=failed&since=24h&limit=200", enc(&prov))).await {
                let mut seen = std::collections::HashSet::new();
                errors.set(x["signIns"].as_array().cloned().unwrap_or_default().into_iter().filter(|r| s(r, "app") == cid && ["AADSTS50011", "AADSTS7000215", "AADSTS7000222", "AADSTS7000218", "AADSTS65001", "AADSTS700016", "AADSTS54005", "AADSTS501481"].contains(&s(r, "code").as_str()) && seen.insert(s(r, "code"))).take(3).collect());
            }
        });
    });
    let put = move |b: Value, msg: &'static str| { let (prov, cid) = (p.get_untracked(), s(&draft.get_untracked(), "client_id")); leptos::task::spawn_local(async move {
        match api::send("PUT", &format!("/identity/{}/apps/{}", enc(&prov), enc(&cid)), b).await { Ok(_) => ui::toast(msg), Err(e) => ui::toast_err(e) } }); };
    let save = move |_| { let d = draft.get_untracked(); put(json!({"name": d["name"], "type": d["type"], "redirect_uris": d["redirect_uris"], "any_secret": d["any_secret"], "permissions": d["permissions"], "optional_claims": d["optional_claims"], "multi_tenant": d["multi_tenant"]}), "Saved in the provider file"); };
    let grant = move |_| { let (prov, cid) = (p.get_untracked(), s(&cur.get_untracked(), "client_id")); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/identity/{}/apps/{}/admin-consent", enc(&prov), enc(&cid)), json!({})).await { Ok(_) => ui::toast("Admin consent granted: the login page stops asking"), Err(e) => ui::toast_err(e) } }); };
    let new_uri = RwSignal::new(String::new());
    let reg_open = RwSignal::new(false);
    let (r_id, r_type, r_uris) = (RwSignal::new(String::new()), RwSignal::new("web".to_string()), RwSignal::new(String::new()));
    let register = move |_| {
        let (cid, ty, uris, prov) = (r_id.get_untracked().trim().to_string(), r_type.get_untracked(), r_uris.get_untracked(), p.get_untracked());
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            let u: Vec<String> = uris.split([',', '\n', ' ']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            match api::send("PUT", &format!("/identity/{}/apps/{}", enc(&prov), enc(&cid)), json!({"type": ty, "redirect_uris": u})).await {
                Ok(_) => { reg_open.set(false); ui::toast(format!("{cid} registered")); nav(&href("apps", &prov, &format!("&app={}", enc(&cid))), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let del = RwSignal::new(false);
    let do_del = move |_| { let (prov, cid) = (p.get_untracked(), s(&cur.get_untracked(), "client_id")); let nav = use_navigate(); leptos::task::spawn_local(async move {
        match api::send("DELETE", &format!("/identity/{}/apps/{}", enc(&prov), enc(&cid)), Value::Null).await { Ok(_) => { ui::toast(format!("{cid} removed")); nav(&href("apps", &prov, ""), Default::default()); } Err(e) => ui::toast_err(e) } }); };
    view! {
        <div class="page">
            <Head active="apps" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"1 unsaved change"</span><button class="btn" on:click=move |_| draft.set(cur.get_untracked())>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button>
            }/>
            <div class="body">
                <div class="list narrow kvcol">
                    <div class="caps" style="padding:16px 20px 6px">"App registrations"</div>
                    {move || data.get()["apps"].as_array().cloned().unwrap_or_default().into_iter().map(|a| {
                        let id = s(&a, "client_id"); let (i2, i3) = (id.clone(), id.clone()); let nav = nav.clone();
                        let sub = if s(&a, "type") == "api" { format!("exposes an API  ·  {} scope(s)", list(&a["exposes"]).len()) } else { kind_label(&s(&a, "type")).to_string() };
                        view! { <div class="kvrow" class:on=move || s(&cur.get(), "client_id") == i2 on:click=move |_| nav(&href("apps", &p.get_untracked(), &format!("&app={}", enc(&i3))), Default::default())>
                            <div>{s(&a, "name")}</div><div class="small cap">{sub}</div></div> }
                    }).collect_view()}
                    <div style="padding:10px 20px"><button class="link" on:click=move |_| { r_id.set(String::new()); r_uris.set(String::new()); reg_open.set(true); }>"+ Register an app"</button></div>
                </div>
                {move || {
                    let d = draft.get();
                    if d.is_null() { return view! { <div class="pane pad"><div class="callout"><div class="t">"No apps yet"</div><div class="t2">"Register one: name, type (web with a secret, single-page with PKCE, daemon for client credentials, API that exposes scopes) and its redirect URIs."</div></div></div> }.into_any(); }
                    let t = tenant.get();
                    let cid = s(&d, "client_id");
                    let ty = s(&d, "type");
                    let api_scopes: Vec<String> = list(&d["permissions"]).into_iter().filter(|x| x.starts_with("api://")).collect();
                    let missing_admin: Vec<String> = api_scopes.iter().filter(|x| !list(&d["admin_consent"]).contains(x)).cloned().collect();
                    let secret = s(&d, "secret");
                    view! {
                        <div class="pane pad"><div style="max-width:640px">
                            <h2 style="margin:0">{s(&d, "name")}</h2>
                            <div class="small cap">{format!("Client id {cid}  ·  {}", if d["multi_tenant"] == json!(true) { "multi-tenant".to_string() } else { format!("single tenant ({})", s(&t, "tenant")) })}</div>
                            <div style="margin-top:14px">{errors.get().into_iter().map(|r| { let code = s(&r, "code"); let uri = s(&r, "sentence").split("come back to ").nth(1).and_then(|x| x.split(',').next()).unwrap_or("").to_string(); let u2 = uri.clone(); view! {
                                <div class="callout danger-l" style="max-width:none;margin:0 0 10px"><div class="t">{format!("{code}  ·  {}", api::ago(api::n(&r, "at")))}</div><div class="t2" style="margin:0">{s(&r, "sentence")}". "{code_note(&code)}</div>
                                    {(code == "AADSTS50011" && !uri.is_empty()).then(|| view! { <button class="link" style="margin-top:6px" on:click=move |_| { let u = u2.split_whitespace().next().unwrap_or("").to_string(); draft.update(|d| { let mut l = list(&d["redirect_uris"]); if !l.contains(&u) { l.push(u); } d["redirect_uris"] = json!(l); }); }>"Add this URI"</button> })}</div> } }).collect_view()}</div>
                            {(!missing_admin.is_empty()).then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 10px"><div class="t">{missing_admin.join(", ")}</div>
                                <div class="t2" style="margin:0">"Users see \"need admin approval\" on the login page; grant it here to continue."</div><button class="link" style="margin-top:6px" on:click=grant>"Grant admin consent"</button></div> })}
                            <div class="caps" style="margin-top:14px">"Type"</div>
                            <select class="select" style="margin-top:6px" on:change=move |e| draft.update(|d| d["type"] = json!(event_target_value(&e)))>
                                {[("web", "Web app"), ("spa", "Single-page app"), ("daemon", "Daemon / service"), ("api", "API")].into_iter().map(|(v, l)| { let sel = ty == v; view! { <option value=v selected=sel>{l}</option> } }).collect_view()}</select>
                            {(ty != "api" && ty != "daemon").then(|| view! {
                                <div class="caps" style="margin-top:22px">"Redirect URIs"</div>
                                <table class="kv">{list(&d["redirect_uris"]).into_iter().map(|u| { let u2 = u.clone(); view! { <tr><td class="mono" style="font:400 12px var(--mono);color:var(--ink);width:auto">{u}</td><td style="width:20px"><button class="link plain" on:click=move |_| draft.update(|d| { let mut l = list(&d["redirect_uris"]); l.retain(|x| *x != u2); d["redirect_uris"] = json!(l); })>"×"</button></td></tr> } }).collect_view()}</table>
                                <div class="row" style="gap:8px;margin-top:8px"><input class="input mono grow" placeholder="http://localhost:5173/signin-oidc" prop:value=move || new_uri.get() on:input=move |e| new_uri.set(event_target_value(&e))/>
                                    <button class="link" on:click=move |_| { let u = new_uri.get_untracked().trim().to_string(); if !u.is_empty() { draft.update(|d| { let mut l = list(&d["redirect_uris"]); l.push(u); d["redirect_uris"] = json!(l); }); new_uri.set(String::new()); } }>"+ Add redirect URI"</button></div>
                                <div class="small cap" style="margin-top:4px">"exact match; a different one gets AADSTS50011"</div>
                            })}
                            {(ty != "spa" && ty != "api").then(|| { let sec = secret.clone(); view! {
                                <div class="caps" style="margin-top:22px">"Credentials"</div>
                                <table class="kv2" style="margin-top:6px"><tr><td style="width:120px">"Client secret"</td><td>{if let Some(n) = sec.strip_prefix("secret://") { view! { <a class="link" href=format!("/secrets?item={}", enc(n))>{format!("from Secrets › {n}")}</a> }.into_any() } else if sec.is_empty() { view! { <span class="cap">"none"</span> }.into_any() } else { view! { <span class="mono">"literal in the file"</span> }.into_any() }}</td></tr>
                                    <tr><td>"Certificate"</td><td class="cap">"none"</td></tr></table>
                                <div class="row" style="gap:12px;margin-top:10px"><Toggle on=Signal::derive(move || draft.get()["any_secret"] == json!(true)) set=Callback::new(move |b: bool| draft.update(|d| d["any_secret"] = json!(b)))/><span>"Accept any client secret"</span></div>
                            } })}
                            {(ty != "api").then(|| view! {
                                <div class="caps" style="margin-top:22px">"API permissions"</div>
                                <table class="kv">{list(&d["permissions"]).into_iter().map(|x| { let admin = list(&d["admin_consent"]).contains(&x); let api = x.starts_with("api://"); let x2 = x.clone(); view! {
                                    <tr><td class="mono" style="font:400 12px var(--mono);color:var(--ink);width:auto">{x}</td><td class="small cap">{if admin { "admin consent given".to_string() } else if api { "needs admin consent".to_string() } else { format!("consented by {} user(s)", api::n(&d, "consentedBy")) }}</td>
                                        <td style="width:20px"><button class="link plain" on:click=move |_| draft.update(|d| { let mut l = list(&d["permissions"]); l.retain(|y| *y != x2); d["permissions"] = json!(l); })>"×"</button></td></tr> } }).collect_view()}</table>
                            })}
                            {(ty == "api").then(|| view! { <div class="caps" style="margin-top:22px">"Exposes"</div><div class="mono small" style="margin-top:6px">{list(&d["exposes"]).iter().map(|x| format!("{}/{x}", s(&d, "app_id_uri"))).collect::<Vec<_>>().join("  ·  ")}</div> })}
                            <div class="caps" style="margin-top:22px">"Optional claims"</div>
                            <div class="row" style="gap:6px;margin-top:6px;flex-wrap:wrap">{["groups", "email", "employeeId", "ctry"].into_iter().map(|c| view! {
                                <button class="chip" class:on=move || list(&draft.get()["optional_claims"]).contains(&c.to_string()) on:click=move |_| draft.update(|d| { let mut l = list(&d["optional_claims"]); if l.contains(&c.to_string()) { l.retain(|x| x != c) } else { l.push(c.into()) } d["optional_claims"] = json!(l); })>{move || format!("{}{c}", if list(&draft.get()["optional_claims"]).contains(&c.to_string()) { "✓ " } else { "+ " })}</button> }).collect_view()}</div>
                            <div class="row" style="gap:12px;margin-top:16px"><Toggle on=Signal::derive(move || draft.get()["multi_tenant"] == json!(true)) set=Callback::new(move |b: bool| draft.update(|d| d["multi_tenant"] = json!(b)))/><span>"Accounts in other tenants may sign in"</span></div>
                            <div style="margin-top:22px"><button class="link danger" on:click=move |_| del.set(true)>"Remove registration"</button></div>
                        </div></div>
                        <div class="side">
                            <div class="caps">"Tokens this app gets"</div>
                            <table class="kv">
                                {(ty != "daemon" && ty != "api").then(|| view! { <tr><td class="mono">"id_token"</td><td class="small">{format!("aud {cid}  ·  {}", s(&t["lifetimes"], "id"))}</td></tr> })}
                                {(ty != "api").then(|| view! { <tr><td class="mono">"access_token"</td><td class="small">{format!("{}  ·  {}", if api_scopes.is_empty() { "aud Microsoft Graph".to_string() } else { format!("aud {}  ·  scp {}", api_scopes[0].rsplit_once('/').map(|x| x.0).unwrap_or(""), api_scopes.iter().filter_map(|x| x.rsplit('/').next()).collect::<Vec<_>>().join(" ")) }, s(&t["lifetimes"], "access"))}</td></tr> })}
                                {list(&d["permissions"]).contains(&"offline_access".to_string()).then(|| view! { <tr><td class="mono">"refresh_token"</td><td class="small">{format!("{}, rotated on use", s(&t["lifetimes"], "refresh"))}</td></tr> })}
                            </table>
                            <div class="caps" style="margin-top:24px">"Endpoints for this app"</div>
                            {move || { let t = tenant.get(); let (a, d2) = (s(&t, "issuer"), s(&t, "discovery")); let (a2, d3) = (a.clone(), d2.clone()); view! {
                                <table class="kv"><tr><td>"Authority"</td><td class="mono small" style="word-break:break-all">{a}</td><td><button class="link" on:click=move |_| ui::copy(&a2, "Authority")>"Copy"</button></td></tr>
                                <tr><td>"Discovery"</td><td class="mono small" style="word-break:break-all">{d2}</td><td><button class="link" on:click=move |_| ui::copy(&d3, "Discovery")>"Copy"</button></td></tr></table> } }}
                            <div class="small cap" style="margin-top:8px">"The app's Authority setting is the only change it needs (HTTP: turn off RequireHttpsMetadata in development)."</div>
                            <div style="margin-top:12px"><a class="link" href=href("tenant", &p.get_untracked(), "")>"Tenant settings ›"</a></div>
                        </div>
                    }.into_any()
                }}
            </div>
            <Dialog open=reg_open title="Register an app">
                <div class="fd">
                    <div class="small t2">"Who may ask for tokens and where they come back."</div>
                    <div class="caps fd-l">"Client id"</div><input class="input mono" style="width:100%" placeholder="acme-app" prop:value=move || r_id.get() on:input=move |e| r_id.set(event_target_value(&e))/>
                    <div class="caps fd-l">"Type"</div>
                    <div class="fd-cards two">{[("web", "Web app", "server-side, with a client secret"), ("spa", "Single-page app", "public, PKCE, no secret"), ("daemon", "Daemon / service", "client credentials, no person"), ("api", "API", "exposes scopes to other apps")].into_iter().map(|(v, l, dsc)| view! {
                        <button class="fd-card" class:on=move || r_type.get() == v on:click=move |_| r_type.set(v.into())><span class="fd-radio"></span><b>{l}</b><span class="small t2">{dsc}</span></button> }).collect_view()}</div>
                    {move || (r_type.get() == "web" || r_type.get() == "spa").then(|| view! {
                        <div class="caps fd-l">"Redirect URIs"</div><textarea class="input mono" style="width:100%;min-height:60px;padding:8px 10px" placeholder="http://localhost:5173/signin-oidc" prop:value=move || r_uris.get() on:input=move |e| r_uris.set(event_target_value(&e))></textarea>
                        <div class="small cap">"One per line; exact match."</div> })}
                    <div class="fd-foot"><span class="small cap grow">{move || if r_type.get() == "web" || r_type.get() == "daemon" { "A synthetic client secret goes to Secrets." } else { "" }}</span>
                        <button class="btn" on:click=move |_| reg_open.set(false)>"Cancel"</button><button class="btn primary" on:click=register>"Register"</button></div>
                </div>
            </Dialog>
            <Confirm open=del question=Signal::derive(move || format!("Remove {}?", s(&cur.get(), "client_id"))) detail="Its sign-ins and token requests are refused (AADSTS700016)." action="Remove" on_yes=do_del/>
        </div>
    }
}
