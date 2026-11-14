//! I1 · Sign-ins: every sign-in and token issued, as sentences, with the steps, the claims, the tokens
//! decoded, and what the next sign-ins will do. Pages waiting for a person show on top.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{code_note, href, providers, result_class, sel, Head, NewProvider};
use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

#[component]
pub fn SignIns() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let qm = use_query_map();
    let all = providers();
    let p = sel();
    let since = RwSignal::new("1h".to_string());
    let search = RwSignal::new(String::new());
    let rows = RwSignal::new(Vec::<Value>::new());
    let pending = RwSignal::new(Vec::<Value>::new());
    let picked = RwSignal::new(Value::Null);
    let new_open = RwSignal::new(false);
    let decoded_open = RwSignal::new(false);
    Effect::new(move |_| {
        live.tick.track();
        let (prov, sn) = (p.get(), since.get());
        let user = qm.get().get("user").unwrap_or_default();
        if prov.is_empty() { return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/identity/sign-ins?provider={}&since={sn}&user={}&limit=300", enc(&prov), enc(&user))).await { rows.set(x["signIns"].as_array().cloned().unwrap_or_default()); }
            if let Ok(x) = api::get(&format!("/identity/pending?provider={}", enc(&prov))).await { pending.set(x["pending"].as_array().cloned().unwrap_or_default()); }
        });
    });
    // The open sign-in follows ?id=, else the newest.
    Effect::new(move |_| {
        let id = qm.get().get("id").and_then(|x| x.parse::<i64>().ok()).or_else(|| rows.get().first().map(|r| api::n(r, "id")));
        let Some(id) = id else { picked.set(Value::Null); return };
        if api::n(&picked.get_untracked(), "id") == id { return; }
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/identity/sign-ins/{id}")).await { picked.set(x); } });
    });
    let shown = Memo::new(move |_| { let q = search.get().to_lowercase(); rows.get().into_iter().filter(|r| q.is_empty() || r.to_string().to_lowercase().contains(&q)).collect::<Vec<_>>() });
    let cur = Memo::new(move |_| super::current(&all.get(), Some(p.get())));
    let provoke = move |e: leptos::ev::Event| {
        let a = event_target_value(&e);
        let prov = p.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/identity/{}/provoke", enc(&prov)), json!({"answer": a})).await { Ok(_) => ui::toast(if a == "normal" { "Next sign-ins: normal".to_string() } else { format!("Next sign-ins: {}", a.replace('_', " ")) }), Err(e) => ui::toast_err(e) }
        });
    };
    let cancel = move |id: String| leptos::task::spawn_local(async move { match api::send("POST", &format!("/identity/pending/{}/cancel", enc(&id)), json!({})).await { Ok(_) => ui::toast("Cancelled: the app gets error=access_denied"), Err(e) => ui::toast_err(e) } });
    view! {
        <div class="page">
            <Head active="signins" right=move || view! {
                <select class="select" on:change=move |e| since.set(event_target_value(&e))>
                    {[("1h", "Last hour"), ("24h", "Last 24 h"), ("7d", "Last 7 days")].into_iter().map(|(v, l)| view! { <option value=v selected=move || since.get() == v>{l}</option> }).collect_view()}</select>
                <input class="input" style="width:180px" placeholder="User, app, error…" prop:value=move || search.get() on:input=move |e| search.set(event_target_value(&e))/>
                <span class="small" style="display:flex;align-items:center;gap:6px"><span class="dot"></span>"Live"</span>
            }/>
            <div class="body">
                <div class="list narrow kvcol">
                    <div class="row" style="padding:16px 20px 6px"><span class="caps grow">"Identity providers"</span><button class="link" on:click=move |_| new_open.set(true)>"+ New"</button></div>
                    {move || all.get()["providers"].as_array().cloned().unwrap_or_default().into_iter().map(|x| {
                        let n = s(&x, "name"); let (n2, n3) = (n.clone(), n.clone()); let nav = nav.clone();
                        view! { <div class="kvrow" class:on=move || p.get() == n2 on:click=move |_| nav(&href("signins", &n3, ""), Default::default())>
                            <div class="row"><span class="grow">{s(&x, "label")}</span><span class="mono small cap">{api::n(&x, "signInsLastHour")}</span></div>
                            <div class="small cap">{format!("tenant {}", s(&x, "tenant"))}</div></div> }
                    }).collect_view()}
                    <div class="small cap" style="padding:12px 20px">"Each provider has its own users, apps and tenant."</div>
                    <div style="padding:0 20px 8px"><a class="link" href="/oauth-clients">"OAuth clients ›"</a><div class="small cap">"Stuntman as the client of your app's OAuth / MCP server"</div></div>
                    {move || { let pd = pending.get(); (!pd.is_empty()).then(|| view! {
                        <div class="caps" style="padding:16px 20px 4px">"Waiting for a person"</div>
                        {pd.into_iter().map(|x| { let id = s(&x, "id"); view! {
                            <div style="padding:6px 20px"><div class="small">{format!("{}  ·  {}", s(&x, "client_id"), api::ago(api::n(&x, "at")))}</div>
                                <div class="small cap ell">{s(&x, "scope")}</div>
                                <button class="link" on:click=move |_| cancel(id.clone())>"Cancel for the person"</button></div> } }).collect_view()}
                        <div class="small cap" style="padding:4px 20px">"Agents complete them with sign_in_as."</div>
                    }) }}
                </div>
                <div class="pane files">
                    {move || (rows.get().is_empty() && !p.get().is_empty()).then(|| { let auth = s(&cur.get(), "authority"); let a2 = auth.clone(); view! {
                        <div class="callout" style="margin:20px 24px"><div class="t">"No sign-ins yet"</div>
                            <div class="t2">{format!("Set the app's Authority to {auth}")}</div><button class="link" on:click=move |_| ui::copy(&a2, "Authority")>"Copy"</button></div> } })}
                    <div class="ftable">
                        {move || shown.get().into_iter().map(|r| {
                            let id = api::n(&r, "id");
                            let prov = p.get_untracked();
                            let nav = use_navigate();
                            let code = s(&r, "code");
                            let res = if code.is_empty() { s(&r, "result") } else { code };
                            view! { <div class="frow" class:on=move || api::n(&picked.get(), "id") == id on:click=move |_| nav(&href("signins", &prov, &format!("&id={id}")), Default::default())>
                                <span class="mono small cap" style="width:80px">{api::hms(api::n(&r, "at"))}</span>
                                <span class="grow"><div>{s(&r, "sentence")}</div><div class=format!("small {}", if s(&r, "result").contains("provoked") { "attn" } else { result_class(&r) })>{res}</div></span></div> }
                        }).collect_view()}
                    </div>
                    <div class="note">"\"The app refused her\" comes from the app's trace (Telemetry), not from the provider."</div>
                </div>
                <div class="side">
                    {move || {
                        let x = picked.get();
                        let provoked = current_provoked(&all.get(), &p.get());
                        let next = view! {
                            <div class="caps" style="margin-top:28px">"Next sign-ins"</div>
                            <select class="select" style="margin-top:8px" on:change=provoke>
                                {[("normal", "Normal"), ("mfa_required", "Require MFA"), ("mfa_fails", "MFA fails"), ("consent_required", "Consent required"), ("user_cancels", "User cancels"), ("wrong_tenant", "Wrong tenant"), ("expired", "Token already expired"), ("bad_signature", "Bad signature"), ("idp_down", "IdP down")]
                                    .into_iter().map(|(v, l)| { let pv = provoked.clone(); view! { <option value=v selected=pv == v || (v == "normal" && pv.is_empty())>{l}</option> } }).collect_view()}
                            </select>
                            <div class="small cap" style="margin-top:8px">"or provoke: MFA required or failing, consent required, user cancels, wrong tenant, token already expired, bad signature, IdP down."</div>
                            <div class="row" style="gap:16px;margin-top:12px"><a class="link" href=href("users", &p.get_untracked(), "")>"Users ›"</a><a class="link" href=href("tenant", &p.get_untracked(), "")>"Tenant settings ›"</a></div>
                        };
                        if x.is_null() { return view! { <div class="small cap">"Pick a sign-in to see its steps and claims."</div>{next} }.into_any(); }
                        let code = s(&x, "code");
                        let note = code_note(&code);
                        let claims = x["claims"].clone();
                        let idt = s(&x["tokens"], "id_token");
                        let i2 = idt.clone();
                        let keys = ["sub", "oid", "email", "tid", "groups", "_claim_names", "roles", "amr", "aud", "exp"];
                        view! {
                            <div style="font-weight:600;font-size:15px">{s(&x, "sentence")}</div>
                            <div class="small cap">{let f = s(&x, "flow"); if f.is_empty() { s(&x, "kind") } else { f }}</div>
                            {(!code.is_empty()).then(|| view! { <div class="callout danger-l" style="max-width:none;margin:14px 0 0"><div class="t">{code.clone()}</div><div class="t2" style="margin:0">{note}</div></div> })}
                            <div class="caps" style="margin-top:20px">"Steps"</div>
                            <div style="margin-top:8px">{x["steps"].as_array().cloned().unwrap_or_default().into_iter().map(|st| view! {
                                <div class="row" style="gap:12px;align-items:baseline;padding:4px 0"><span class="mono small cap" style="width:64px;flex:none">{api::hms(api::n(&st, "at"))}</span>
                                    <div style="flex:1;min-width:0"><div>{s(&st, "what")}</div><div class="small cap" style="overflow-wrap:anywhere">{s(&st, "detail")}</div></div></div> }).collect_view()}</div>
                            {claims.is_object().then(|| view! {
                                <div class="caps" style="margin-top:20px">"ID token claims"</div>
                                <table class="kv">{keys.into_iter().filter(|k| !claims[*k].is_null()).map(|k| { let v = &claims[k]; let shown = match v { Value::Array(a) => a.iter().map(|y| y.as_str().map(String::from).unwrap_or(y.to_string())).collect::<Vec<_>>().join(", "), Value::String(t) => t.clone(), o => o.to_string() };
                                    view! { <tr><td class="mono" style="width:96px;vertical-align:top">{k}</td><td class="mono" style="overflow-wrap:anywhere">{shown}</td></tr> } }).collect_view()}</table>
                                <div class="row" style="gap:10px;margin-top:12px">
                                    <button class="btn" on:click=move |_| decoded_open.set(true)>"Decode all tokens"</button>
                                    {(!idt.is_empty()).then(|| view! { <button class="btn" on:click=move |_| ui::copy(&i2, "id_token")>"Copy id_token"</button> })}
                                </div>
                            })}
                            {next}
                        }.into_any()
                    }}
                </div>
            </div>
            <Dialog open=decoded_open title="Tokens of this sign-in, decoded">
                <pre class="yaml" style="max-height:70vh;overflow:auto;white-space:pre-wrap;word-break:break-all;width:640px;max-width:100%">{move || serde_json::to_string_pretty(&picked.get()["decoded"]).unwrap_or_default()}</pre>
                <div class="small cap">"Decoded only: the signature is checked by the app against the JWKS."</div>
            </Dialog>
            <NewProvider open=new_open/>
        </div>
    }
}

fn current_provoked(all: &Value, p: &str) -> String {
    s(&super::current(all, Some(p.to_string()))["provoked"], "answer")
}
