//! K4 · Vault settings: identities with roles and scopes, any token, the managed identity endpoint,
//! throttling, latency, soft delete, subscribers and seed; the vault YAML with the changed lines.

use leptos::prelude::*;
use serde_json::{json, Value};

use super::sel;
use crate::{
    api::{self, enc, s},
    ui::{self, Toggle},
    whatsapp::YamlMarked,
};

#[component]
pub fn Settings() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let (vault, _) = sel();
    let data = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let before = RwSignal::new(String::new());
    let load = move || {
        let v = vault.get_untracked();
        if v.is_empty() { return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/keyvault/vaults/{}", enc(&v))).await {
                let old = s(&data.get_untracked(), "yaml");
                if !old.is_empty() && old != s(&x, "yaml") && s(&data.get_untracked()["vault"], "name") == v { before.set(old); } else if s(&data.get_untracked()["vault"], "name") != v { before.set(String::new()); }
                draft.set(x["vault"].clone());
                data.set(x);
            }
        });
    };
    Effect::new(move |_| { vault.track(); load(); });
    // K4 · File changed elsewhere: while you edit, a change by the API, MCP or the file is announced, not merged silently.
    let elsewhere = RwSignal::new(false);
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic") == "keyvault.vault" {
        if draft.get_untracked() == data.get_untracked()["vault"] { load(); } else { elsewhere.set(true); } } t });
    // K4 · what the vault's settings cause right now: reads only any-token lets through, refusals, IMDS, deleted names.
    let wf = RwSignal::new(0usize);
    let findings = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        let v = vault.get();
        if v.is_empty() { return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/keyvault/would-fail?vault={}&since=24h", enc(&v))).await { wf.set(x["operations"].as_array().map(|a| a.len()).unwrap_or(0)); }
            if let Ok(x) = api::get(&format!("/keyvault/patterns?vault={}", enc(&v))).await { findings.set(x["findings"].as_array().cloned().unwrap_or_default()); }
        });
    });
    let recover = move |kind: &'static str, n: String| { let v = vault.get_untracked(); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/keyvault/recover/{}", enc(&n)), json!({"kind": kind, "vault": v})).await { Ok(_) => { ui::toast(format!("{n} recovered")); load(); } Err(e) => ui::toast_err(e) } }); };
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != data.get()["vault"]);
    let save = move |_| {
        let d = draft.get_untracked();
        let n = s(&d, "name");
        leptos::task::spawn_local(async move {
            let b = json!({"any_token": d["access"]["any_token"], "identities": d["access"]["identities"], "throttle": d["behaviour"]["throttle"], "latency": d["behaviour"]["latency"],
                "soft_delete_days": d["behaviour"]["soft_delete_days"], "seed_on_reset": d["behaviour"]["seed_on_reset"], "subscribers": d["subscribers"]});
            match api::send("PUT", &format!("/keyvault/vaults/{}", enc(&n)), b).await {
                Ok(_) => { ui::toast(format!("Saved in secrets/{n}.yaml")); elsewhere.set(false); load(); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let set = move |path: &'static str, v: Value| draft.update(|d| { if let Some(p) = d.pointer_mut(path) { *p = v; } });
    let role_menu = RwSignal::new(false);
    let roles_all = Memo::new(move |_| data.get()["roles"].as_array().cloned().unwrap_or_default());
    let role_lbl = move |id: &str| roles_all.get_untracked().iter().find(|r| s(r, "id") == id).map(|r| s(r, "label")).unwrap_or(id.to_string());
    let (a_id, a_roles, a_scope, adding) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(false));
    let add = move |_| {
        let id = a_id.get_untracked().trim().to_string();
        if id.is_empty() { return; }
        let split = |x: String| x.split(',').map(|y| y.trim().to_string()).filter(|y| !y.is_empty()).collect::<Vec<_>>();
        let (roles, scope) = (split(a_roles.get_untracked()), split(a_scope.get_untracked()));
        draft.update(|d| { if let Some(a) = d["access"]["identities"].as_array_mut() { a.retain(|i| s(i, "id") != id); a.push(json!({"id": id, "roles": roles, "scope": scope})); } });
        a_id.set(String::new()); a_roles.set(String::new()); a_scope.set(String::new()); adding.set(false);
    };
    let imds = move |e: leptos::ev::Event| {
        let a = event_target_value(&e);
        leptos::task::spawn_local(async move { match api::send("POST", "/keyvault/imds/provoke", json!({"answer": a, "by": "you"})).await { Ok(_) => ui::toast(format!("Managed identity endpoint: {a}")), Err(e) => ui::toast_err(e) } });
    };
    let sel_box = move |path: &'static str, opts: Vec<(&'static str, &'static str)>| view! {
        <select class="select" style="width:180px" on:change=move |e| set(path, json!(event_target_value(&e)))>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || draft.get().pointer(path).and_then(|x| x.as_str().map(String::from)).unwrap_or_default() == v>{l}</option> }).collect_view()}
        </select>
    };
    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <div class="titled">
                        <div class="row" style="gap:14px;align-items:baseline">
                            <a class="crumb-l" href=move || format!("/secrets?vault={}", enc(&vault.get()))>"Key Vault"</a><span class="cap">"/"</span>
                            <h1>{move || vault.get()}</h1>
                            <span class="small cap">{move || format!("Azure Key Vault in {}", s(&env.get(), "env").to_uppercase())}</span>
                        </div>
                    </div>
                    <span class="grow"></span>
                    {move || dirty.get().then(|| view! { <span class="small attn">"1 unsaved change"</span><button class="btn" on:click=move |_| draft.set(data.get_untracked()["vault"].clone())>"Discard"</button> })}
                    <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button>
                </div>
                <div style="padding:0 0 10px"><a class="link small" href=move || format!("/secrets?vault={}", enc(&vault.get()))>"‹ Secrets"</a></div>
            </div>
            <div class="body">
                {move || {
                    if vault.get().is_empty() { return super::no_vault().into_any(); }
                    let d = draft.get();
                    if d.is_null() { return view! { <div class="pane pad cap">"Loading…"</div> }.into_any(); }
                    let x = data.get();
                    let url = s(&x["imds"], "url");
                    let u2 = url.clone();
                    let any = d["access"]["any_token"] == json!(true);
                    let roles = x["roles"].as_array().cloned().unwrap_or_default();
                    let role_label = move |r: &str| roles.iter().find(|x| s(x, "id") == r).map(|x| s(x, "label")).unwrap_or(r.to_string());
                    view! {
                        <div class="pane pad form"><div style="max-width:720px">
                            {move || elsewhere.get().then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">"secrets/"{s(&draft.get_untracked(), "name")}".yaml changed while you were editing"</div>
                                <div class="t2" style="margin:0">"Someone saved it through the API, MCP or the file. Load it, or keep yours and save over it."</div>
                                <div class="row" style="gap:16px;margin-top:8px"><button class="link" on:click=move |_| { elsewhere.set(false); draft.set(Value::Null); load(); }>"Load the new file"</button><button class="link" on:click=save>"Keep mine and save over it"</button></div></div> })}
                            {move || (data.get()["vault"]["access"]["any_token"] == json!(true)).then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">"Everyone can read everything"</div>
                                <div class="t2" style="margin:0">"Quick to start, but it hides missing roles: what works here fails in a real vault. Stuntman marks each read that would have been refused."</div>
                                {(wf.get() > 0).then(|| view! { <a class="link" style="margin-top:8px;display:inline-block" href=format!("/secrets/log?vault={}&result=would_fail", enc(&vault.get_untracked()))>{format!("Show {} read{} that would fail ›", wf.get(), if wf.get() == 1 { "" } else { "s" })}</a> })}</div> })}
                            {move || findings.get().into_iter().filter(|f| s(f, "kind") == "refused" && api::n(f, "status") == 403).take(2).map(|f| view! {
                                <div class="callout danger-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("{} has no role on {}", s(&f, "identity"), s(&f, "item"))}</div>
                                    <div class="t2" style="margin:0">{format!("{}. Fix the identity's roles or scope below, or the name the app asks for.", s(&f, "text"))}</div></div> }).collect_view()}
                            {move || { let p = s(&data.get()["imds"], "provoked"); (!p.is_empty() && p != "normal").then(|| view! { <div class="callout danger-l" style="max-width:none;margin:0 0 14px">
                                <div class="t">{match p.as_str() { "500" => "IMDS answers 500".to_string(), "slow" => "IMDS answers after 5 s".to_string(), _ => "IMDS has no identity for the app".to_string() }}</div>
                                <div class="t2" style="margin:0">"DefaultAzureCredential then tries other sources and fails. Tests the start-up message and retries of the app."</div></div> }) }}
                            {move || { let del = data.get()["deleted"].clone(); [("secret", "secrets"), ("key", "keys"), ("certificate", "certificates")].into_iter().flat_map(|(kind, k)| del[k].as_array().cloned().unwrap_or_default().into_iter().map(move |d| (kind, d))).take(3).map(|(kind, d)| {
                                let n = d[0].as_str().unwrap_or("").to_string(); let n2 = n.clone(); view! {
                                <div class="callout" style="max-width:none;margin:0 0 14px"><div class="t">{format!("{n} was deleted {}", api::ago(d[1].as_i64().unwrap_or(0)))}</div>
                                    <div class="t2" style="margin:0">"Recreating it gets 409 Conflict until it is purged or recovered, as in Azure."</div>
                                    <button class="link" style="margin-top:8px" on:click=move |_| recover(kind, n2.clone())>"Recover"</button></div> } }).collect_view() }}
                            <div class="caps">"Identities"</div>
                            <div class="row" style="gap:12px;margin-top:12px">
                                <Toggle on=Signal::derive(move || draft.get()["access"]["any_token"] == json!(true)) set=Callback::new(move |b: bool| set("/access/any_token", json!(b)))/>
                                <span>"Accept any token"</span><span class="grow"></span>
                                <span class="small cap">{if any { "on: everyone can read everything; refusals a real vault would give are marked in the log" } else { "off: only these identities, with these roles" }}</span>
                            </div>
                            <div class="ftable" style="margin:14px 0 0">
                                <div class="fhead caps" style="padding:0 0 8px"><span style="width:180px">"Identity"</span><span class="grow">"Role"</span><span style="width:160px">"Scope"</span><span style="width:20px"></span></div>
                                {d["access"]["identities"].as_array().cloned().unwrap_or_default().into_iter().map(|i| {
                                    let id = s(&i, "id");
                                    let rl = i["roles"].as_array().map(|a| a.iter().filter_map(|r| r.as_str()).map(|r| role_label(r)).collect::<Vec<_>>().join(" + ")).unwrap_or_default();
                                    let sc = i["scope"].as_array().map(|a| a.iter().filter_map(|r| r.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                                    view! { <div class="frow" style="padding:9px 0;cursor:default"><span class="mono" style="width:180px">{id.clone()}</span><span class="grow">{rl}</span>
                                        <span style="width:160px">{if sc.is_empty() { "whole vault".into() } else { sc }}</span>
                                        <button class="link plain" style="width:20px" title="Remove" on:click=move |_| draft.update(|d| { if let Some(a) = d["access"]["identities"].as_array_mut() { a.retain(|x| s(x, "id") != id); } })>"×"</button></div> }
                                }).collect_view()}
                            </div>
                            {move || if adding.get() { view! {
                                <div class="row" style="gap:8px;margin-top:10px">
                                    <input class="input mono" style="width:170px" placeholder="acme-api-mi" prop:value=move || a_id.get() on:input=move |e| a_id.set(event_target_value(&e))/>
                                    <span class="anchor grow"><button class="select" style="width:100%;text-align:left" on:click=move |_| role_menu.set(true)>{move || { let r = a_roles.get(); if r.is_empty() { "Roles…".to_string() } else { r.split(", ").map(|x| role_lbl(x)).collect::<Vec<_>>().join(" + ") } }}</button>
                                        <ui::Pop open=role_menu keep=true>{move || roles_all.get().into_iter().map(|r| { let id = s(&r, "id"); let (id2, id3) = (id.clone(), id.clone()); view! {
                                            <div class="mi" on:click=move |_| a_roles.update(|cur| { let mut v: Vec<String> = cur.split(", ").filter(|x| !x.is_empty()).map(String::from).collect(); if v.contains(&id2) { v.retain(|x| *x != id2) } else { v.push(id2.clone()) } *cur = v.join(", "); })>
                                                <span class="grow">{s(&r, "label")}</span><span>{move || if a_roles.get().split(", ").any(|x| x == id3) { "✓" } else { "" }}</span></div> } }).collect_view()}</ui::Pop></span>
                                    <input class="input" style="width:150px" placeholder="smtp-* (empty: all)" prop:value=move || a_scope.get() on:input=move |e| a_scope.set(event_target_value(&e))/>
                                    <button class="btn" on:click=add>"Add"</button>
                                </div>
                            }.into_any() } else { view! {
                                <div class="row" style="gap:16px;margin-top:10px"><button class="link" on:click=move |_| adding.set(true)>"+ Add identity"</button>
                                    <span class="small cap">"the token's oid, appid or name: from the managed identity endpoint below or any JWT"</span></div>
                            }.into_any() }}
                            <div style="border-top:1px solid var(--rule);margin:22px 0"></div>
                            <div class="caps">"Managed identity endpoint"</div>
                            <div class="row" style="gap:16px;margin-top:12px"><span style="width:110px">"IMDS URL"</span><span class="mono grow ell">{url}</span><button class="link" on:click=move |_| ui::copy(&u2, "IMDS URL")>"Copy"</button></div>
                            <div class="small cap" style="margin-top:6px">"DefaultAzureCredential gets a token here, as on an Azure VM or App Service: set IDENTITY_ENDPOINT to it and IDENTITY_HEADER to the identity (e.g. acme-api-mi)."</div>
                            <div class="row" style="gap:16px;margin-top:12px"><span style="width:110px">"Next tokens"</span>
                                <select class="select" on:change=imds>
                                    {[("normal", "Normal"), ("500", "500 error"), ("slow", "Slow (5 s)"), ("no_identity", "No identity")].into_iter().map(|(v, l)| view! { <option value=v selected=s(&x["imds"], "provoked") == v || (v == "normal" && x["imds"]["provoked"].is_null())>{l}</option> }).collect_view()}
                                </select></div>
                            <div style="border-top:1px solid var(--rule);margin:22px 0"></div>
                            <div class="caps">"Behaviour"</div>
                            <div class="row" style="gap:16px;margin-top:12px"><span style="width:160px">"Throttling"</span>{sel_box("/behaviour/throttle", vec![("2000/10s", "2,000 per 10 s"), ("200/10s", "200 per 10 s"), ("10/10s", "10 per 10 s"), ("off", "Off")])}<span class="small cap">"over it: 429 with Retry-After"</span></div>
                            <div class="row" style="gap:16px;margin-top:10px"><span style="width:160px">"Latency"</span>{sel_box("/behaviour/latency", vec![("none", "None"), ("100ms", "100 ms"), ("300ms", "300 ms"), ("2s", "2 s")])}<span class="small cap">"added to every answer"</span></div>
                            <div class="row" style="gap:16px;margin-top:10px"><span style="width:160px">"Soft delete"</span>
                                <select class="select" style="width:180px" on:change=move |e| set("/behaviour/soft_delete_days", json!(event_target_value(&e).parse::<u64>().unwrap_or(90)))>
                                    {[7u64, 30, 90].into_iter().map(|n| view! { <option value=n.to_string() selected=move || draft.get()["behaviour"]["soft_delete_days"] == json!(n)>{format!("{n} days")}</option> }).collect_view()}
                                </select><span class="small cap">"deleted names stay taken; recreating gets 409 Conflict"</span></div>
                            <div class="row" style="gap:12px;margin-top:12px">
                                <Toggle on=Signal::derive(move || draft.get()["behaviour"]["seed_on_reset"] == json!(true)) set=Callback::new(move |b: bool| set("/behaviour/seed_on_reset", json!(b)))/>
                                <span>{format!("Load {} on reset", s(&d, "seed"))}</span></div>
                            <div class="row" style="gap:16px;margin-top:14px"><span style="width:160px">"Event Grid subscribers"</span>
                                <input class="input grow" placeholder="http://localhost:5050/hooks/keyvault" prop:value=d["subscribers"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
                                    on:change=move |e| set("/subscribers", json!(event_target_value(&e).split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>()))/></div>
                            <div class="small cap" style="margin-top:6px">"SecretNewVersionCreated, SecretNearExpiry, SecretExpired and the key and certificate events, posted as Event Grid would."</div>
                            <div style="border-top:1px solid var(--rule);margin:22px 0"></div>
                            <div class="small cap">{format!("Fields come from secrets/{}.yaml. Values live in the seed and in the vault, never in this file.", s(&d, "name"))}</div>
                        </div></div>
                        <div class="side yaml" style="width:420px">
                            <div class="row"><span class="caps">{format!("secrets/{}.yaml", s(&d, "name"))}</span><span class="small ok" style="margin-left:10px">"live"</span></div>
                            <YamlMarked text=Signal::derive(move || s(&data.get(), "yaml")) before=Signal::derive(move || before.get())/>
                        </div>
                    }.into_any()
                }}
            </div>
        </div>
    }
}
