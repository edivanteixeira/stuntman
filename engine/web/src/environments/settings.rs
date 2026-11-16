//! N3 · Environment settings: addresses, callback targets, lifetime and pause, telemetry and MCP
//! endpoints, delete. The form edits environments/<name>/environment.yaml, shown beside it with
//! the changed lines marked.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use super::{act, label, lifetime};
use crate::{
    api::{self, s},
    ui::{self, Toggle},
};

/// Fields the form edits, with the YAML key that shows them.
const FIELDS: [(&str, &str); 6] = [("/baseDomain", "base_domain"), ("/publicUrl", "public_url"), ("/allowedEmailDomains", "allowed_email_domains"),
    ("/callbacks/default", "default"), ("/lifetime", "lifetime"), ("/paused", "paused")];

#[component]
pub fn Settings() -> impl IntoView {
    let p = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let name = move || p.get().get("name").unwrap_or_default();
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let elsewhere = RwSignal::new(Value::Null);
    let del = RwSignal::new(false);
    let typed = RwSignal::new(String::new());
    let gone = RwSignal::new(false);
    let adding = RwSignal::new(false);
    let me = expect_context::<crate::shell::Me>();
    let token = RwSignal::new(String::new());
    Effect::new(move |_| {
        let n = name();
        if me.person().is_some() {
            leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/environments/{n}/token")).await { token.set(s(&v, "token")) } });
        }
    });
    let rotate = move |_| { let n = name(); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/environments/{n}/token"), json!({})).await { Ok(v) => { token.set(s(&v, "token")); ui::toast("New token: the old one stopped") } Err(e) => ui::toast_err(e) }
    }) };
    let use_in_mcp = move |_| { let n = name(); leptos::task::spawn_local(async move {
        let Some(email) = me.person() else { return };
        match api::send("PUT", &format!("/users/{}/current-environment", api::enc(&email)), json!({"environment": n})).await {
            Ok(_) => { me.0.update(|v| v["currentEnvironment"] = json!(n)); ui::toast(format!("Your new MCP sessions start in {n}")) }
            Err(e) => ui::toast_err(e),
        }
    }) };
    let new_domain = RwSignal::new(String::new());

    let dirty = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        FIELDS.iter().filter(|(p, _)| a.pointer(p) != b.pointer(p)).count()
    });
    let load = move |keep: bool| {
        let n = name();
        leptos::task::spawn_local(async move {
            match api::get(&format!("/environments/{n}")).await {
                Ok(v) => {
                    let editing = keep && dirty.get_untracked() > 0;
                    if editing && v["version"] != saved.get_untracked()["version"] {
                        // The file changed while this form has unsaved edits: say so, let the user pick.
                        elsewhere.set(v);
                        return;
                    }
                    if !editing {
                        draft.set(v.clone());
                    }
                    saved.set(v);
                }
                Err(_) => gone.set(true),
            }
        });
    };
    Effect::new(move |_| {
        name();
        load(false);
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "environments" {
            load(true);
        }
        t
    });
    let set = move |path: &'static str, v: Value| draft.update(|d| if let Some(x) = d.pointer_mut(path) { *x = v; });
    let val = move |path: &'static str| draft.get().pointer(path).cloned().unwrap_or(Value::Null);
    let cb_bad = move || { let c = s(&val("/callbacks"), "default"); !c.is_empty() && !c.starts_with("http://") && !c.starts_with("https://") };

    let save = move |force: bool| {
        let (d, sv) = (draft.get_untracked(), saved.get_untracked());
        let mut body = json!({});
        if d["baseDomain"] != sv["baseDomain"] { body["base_domain"] = d["baseDomain"].clone(); }
        if d["publicUrl"] != sv["publicUrl"] { body["public_url"] = d["publicUrl"].clone(); }
        if d["allowedEmailDomains"] != sv["allowedEmailDomains"] { body["allowed_email_domains"] = d["allowedEmailDomains"].clone(); }
        if d["callbacks"] != sv["callbacks"] { body["callbacks"] = d["callbacks"].clone(); }
        if d["lifetime"] != sv["lifetime"] { body["lifetime"] = json!(match s(&d, "lifetime").as_str() { "expires" => "24h".to_string(), l => l.to_string() }); }
        if d["paused"] != sv["paused"] { body["paused"] = d["paused"].clone(); }
        if !force { body["version"] = sv["version"].clone(); }
        let n = name();
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/environments/{n}"), body).await {
                Ok(v) => { saved.set(v.clone()); draft.set(v); err.set(String::new()); elsewhere.set(Value::Null); ui::toast("Saved"); }
                Err(e) if e.contains("changed elsewhere") => {
                    // Show what the file says now; the user picks.
                    if let Ok(v) = api::get(&format!("/environments/{n}")).await { elsewhere.set(v); }
                }
                Err(e) => err.set(e),
            }
        });
    };
    let do_delete = move |_| {
        let n = name();
        let nav = nav.clone();
        let here = n == s(&env.get_untracked(), "env");
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/environments/{n}"), json!({})).await {
                Ok(_) => {
                    ui::toast(format!("{} deleted", label(&n)));
                    if here { api::switch_env("", "/environments") } else { nav("/environments", Default::default()) }
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };

    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <h1><a class="crumb" href="/environments">"Environments"</a><span class="crumb" style="margin:0 12px">"/"</span>{move || label(&name())}</h1>
                    <span class="grow"></span>
                    <a class="btn" href=move || format!("/environments/{}/connections", name())>"Connections"</a>
                    {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                        <button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> }) }}
                    <button class="btn primary" disabled=move || dirty.get() == 0 || cb_bad() on:click=move |_| save(false)>"Save"</button>
                </div>
            </div>
            {move || (!elsewhere.get().is_null()).then(|| {
                let (o, n) = (saved.get(), elsewhere.get());
                let theirs: Vec<&str> = FIELDS.iter().filter(|(p, _)| o.pointer(p) != n.pointer(p)).map(|(_, k)| *k).collect();
                let mine: Vec<&str> = FIELDS.iter().filter(|(p, _)| o.pointer(p) != draft.get().pointer(p)).map(|(_, k)| *k).collect();
                view! {
                    <div class="strip">{format!("environment.yaml changed elsewhere while you were editing. It changed {}. Your change: {}.",
                        if theirs.is_empty() { "something".to_string() } else { theirs.join(", ") }, if mine.is_empty() { "none".to_string() } else { mine.join(", ") })}
                        <button class="link" on:click=move |_| { let v = elsewhere.get_untracked(); saved.set(v.clone()); draft.set(v); elsewhere.set(Value::Null); }>"Load the new file"</button>
                        <button class="link" on:click=move |_| save(true)>"Keep mine and save over it"</button></div>
                }
            })}
            <div class="body">
                <div class="pane pad form"><div style="max-width:780px">
                    {move || {
                        if gone.get() { return view! { <div class="empty"><h3>"No such environment"</h3><a class="link" href="/environments">"All environments ›"</a></div> }.into_any(); }
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let sv = saved.get();
                        let n = name();
                        let a = d["addresses"].clone();
                        let real: Vec<String> = d["realHosts"].as_array().cloned().unwrap_or_default().iter().map(|x| x.as_str().unwrap_or("").to_string()).collect();
                        let here = n == s(&env.get(), "env");
                        let (mcp, mcp2, otlp) = (s(&a, "publicMcp"), s(&a, "localMcp"), s(&a, "otlp"));
                        let (n_hosts, n_reset, n_resume) = (n.clone(), n.clone(), n.clone());
                        view! {
                            {(sv["paused"] == true).then(|| view! {
                                <div class="callout attn-l"><div class="t attn">{format!("{} is paused", label(&n))}</div>
                                    <div class="t2">"503 everywhere; a strip says so on every screen of it."</div>
                                    <button class="link" on:click=move |_| { let n = n_resume.clone(); leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/pause"), json!({"paused": false}), "Resumed").await }) }>"Resume"</button></div> })}
                            {me.person().is_some().then(|| {
                                let mcp = s(&me.0.get_untracked(), "mcp");
                                let snippet = move || format!("{{ \"mcpServers\": {{ \"stuntman-{n}\": {{\n  \"type\": \"http\",\n  \"url\": \"{u}/mcp\",\n  \"headers\": {{\n    \"Authorization\": \"Bearer ${{STUNTMAN_TOKEN}}\" }} }} }} }}",
                                    n = name(), u = s(&saved.get()["addresses"], "public"));
                                view! {
                                <div class="block">
                                    <div class="caps" style="margin-bottom:14px">"Agents and automation"</div>
                                    <div class="row" style="margin-bottom:8px"><span style="width:164px">"People's MCP"</span><span class="mono" style="width:340px">{mcp}</span>
                                        <span class="small cap">"one for everyone"</span></div>
                                    <div class="row" style="margin:0 0 14px 172px;gap:12px">
                                        {move || if s(&me.0.get(), "currentEnvironment") == name() { view! { <span class="chip">"In your MCP"</span> }.into_any() }
                                            else { view! { <button class="btn small" on:click=use_in_mcp>"Use in MCP"</button> }.into_any() }}
                                        <span class="small t2">{move || format!("your new sessions start in {}", if me.0.get()["currentEnvironment"].is_null() { "the one you pick".to_string() } else { s(&me.0.get(), "currentEnvironment") })}</span></div>
                                    <div class="row" style="margin-bottom:6px"><span style="width:164px">"Token for automation"</span>
                                        <span class="input mono" style="width:340px">{move || { let t = token.get(); if t.len() > 12 { format!("{}{}{}", &t[..8], "•".repeat(16), &t[t.len() - 4..]) } else { "none yet: Rotate makes one".into() } }}</span>
                                        <button class="link" style="font-size:13px;margin-left:14px" on:click=move |_| ui::copy(&token.get_untracked(), "Token")>"Copy"</button>
                                        <button class="link" style="font-size:13px;margin-left:12px" on:click=rotate>"Rotate"</button></div>
                                    <div class="small cap" style="margin:6px 0 12px 172px">{move || format!("Works only on /e/{0}/api and /e/{0}/mcp. It goes away with the environment. Rotate gives a new one and the old one stops at once. Never in a log, never in a file you commit.", name())}</div>
                                    <div class="row" style="align-items:flex-start"><span style="width:164px">"A pipeline or an agent alone"</span>
                                        <pre class="mono small" style="margin:0;padding:10px 12px;background:var(--subtle);border-radius:6px;width:520px">{snippet}</pre>
                                        <button class="link" style="font-size:13px;margin-left:12px" on:click=move |_| ui::copy(&snippet(), "MCP settings")>"Copy"</button></div>
                                </div> }
                            })}
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Addresses"</div>
                                <div class="row" style="margin-bottom:10px"><span style="width:164px">"Base domain"</span>
                                    <input class="input mono" style="width:260px" class:changed=move || saved.get()["baseDomain"] != draft.get()["baseDomain"] prop:value=s(&d, "baseDomain")
                                        on:change=move |e| set("/baseDomain", json!(event_target_value(&e).trim().to_lowercase()))/>
                                    <span class="small cap">"each service gets a name under it"</span></div>
                                <div class="row" style="margin-bottom:10px"><span style="width:164px">"Public URL"</span>
                                    <input class="input mono" style="width:340px" class:changed=move || saved.get()["publicUrl"] != draft.get()["publicUrl"] placeholder=s(&a, "public")
                                        prop:value=s(&d, "publicUrl") on:change=move |e| set("/publicUrl", json!(event_target_value(&e).trim()))/>
                                    <span class="small cap">"sign-in, issuer and links in pages use it"</span></div>
                                <div class="row" style="margin-bottom:10px"><span style="width:164px">"Apps reach it at"</span><span class="mono" style="width:340px">{s(&a, "apps")}</span>
                                    <span class="small cap">{if s(&a, "apps").starts_with("<service>") { "by name: no /e/ prefix, no hosts lines" } else { "a base domain of your own lets apps use names" }}</span></div>
                                <div class="row" style="margin-bottom:6px"><span style="width:164px">"Real host names"</span>
                                    <span>{if real.is_empty() { "none: the app uses the names under the base domain".to_string() } else { format!("{} answered with the Stuntman Test CA ({})", real.len(), real.join(", ")) }}</span></div>
                                <div class="row" style="margin-left:172px">
                                    <button class="link" style="font-size:13px" on:click=move |_| { let n = n_hosts.clone(); leptos::task::spawn_local(async move {
                                        match api::get(&format!("/environments/{n}/hosts")).await { Ok(v) => ui::copy(&s(&v, "hosts"), "Hosts entries"), Err(e) => ui::toast_err(e) } }) }>"Copy hosts entries"</button>
                                    <a class="link" style="font-size:13px;margin-left:18px" href=api::url("/api/keyvault/test-ca.pem") download="stuntman-test-ca.pem">"Download the test CA"</a></div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Identity"</div>
                                <div class="row wrap" style="gap:8px"><span style="width:164px">"Real e-mail domains"</span>
                                    {move || draft.get()["allowedEmailDomains"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let dm = x.as_str().unwrap_or("").to_string(); let d2 = dm.clone(); view! {
                                        <span class="chip">{dm}<button class="link cap" style="margin-left:8px" title="Remove" on:click=move |_| draft.update(|d| {
                                            if let Some(a) = d["allowedEmailDomains"].as_array_mut() { a.retain(|v| v.as_str() != Some(&d2)); } })>"×"</button></span> } }).collect_view()}
                                    {move || if adding.get() { view! {
                                        <input class="input mono" style="width:200px" placeholder="example.org" prop:value=move || new_domain.get() on:input=move |e| new_domain.set(event_target_value(&e))
                                            on:keydown=move |e| if e.key() == "Enter" { let v = new_domain.get_untracked().trim().trim_start_matches('@').to_lowercase(); if !v.is_empty() {
                                                draft.update(|d| { if !d["allowedEmailDomains"].is_array() { d["allowedEmailDomains"] = json!([]); } if let Some(a) = d["allowedEmailDomains"].as_array_mut() { if !a.iter().any(|x| x.as_str() == Some(&v)) { a.push(json!(v)); } } }); }
                                                new_domain.set(String::new()); adding.set(false); }/> }.into_any()
                                    } else { view! { <button class="link" style="font-size:13px" on:click=move |_| adding.set(true)>"Add a domain"</button> }.into_any() }}
                                </div>
                                <div class="small cap" style="margin:8px 0 0 172px">"People of these domains can be added to this environment's identity providers, so an app whose database has real accounts signs them in. Every other address stays synthetic only."</div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Callbacks"</div>
                                <div class="row"><span style="width:164px">"Default target"</span>
                                    <input class="input mono grow" class:bad=cb_bad class:changed=move || saved.get()["callbacks"] != draft.get()["callbacks"] placeholder="https://acme-api.stg.example.com"
                                        prop:value=s(&d["callbacks"], "default") on:input=move |e| set("/callbacks/default", json!(event_target_value(&e).trim()))/></div>
                                <div class="small cap" style="margin:6px 0 12px 172px">{move || if cb_bad() { "Start with http:// or https://".to_string() } else { "where services send webhooks when a service has no URL of its own".to_string() }}</div>
                                <div class="row" style="align-items:flex-start"><span style="width:164px">"Per service"</span>
                                    <div class="col grow" style="gap:6px">
                                        {d["services"].as_array().cloned().unwrap_or_default().into_iter().map(|v| { let cb = s(&v, "callback"); view! {
                                            <div class="row"><span style="width:180px">{s(&v, "name")}</span>
                                                <span class="mono small cap">{if cb.is_empty() { "uses the default".to_string() } else { format!("from Service settings: {cb}") }}</span></div> } }).collect_view()}
                                        {d["services"].as_array().map(|x| x.is_empty()).unwrap_or(true).then(|| view! { <span class="small cap">"no service sends callbacks yet"</span> })}
                                    </div></div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Lifetime"</div>
                                <div class="row" style="margin-bottom:12px"><span style="width:164px">"Kept or expires"</span>
                                    <select class="select" class:changed=move || saved.get()["lifetime"] != draft.get()["lifetime"] on:change=move |e| set("/lifetime", json!(event_target_value(&e)))>
                                        {[("kept", "Kept"), ("expires", "Expires"), ("pr_closed", "When the pull request closes")].into_iter().map(|(v, l)| { let on = s(&d, "lifetime") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}
                                    </select>
                                    <span class="small" class:attn=sv["lifetime"] == "expires" class:cap=sv["lifetime"] != "expires">{if sv["lifetime"] == "expires" { format!("{} {} · {}", super::day(api::n(&sv, "expiresAt")), api::hm(api::n(&sv, "expiresAt")), lifetime(&sv)) } else { lifetime(&sv) }}</span>
                                    {(sv["lifetime"] == "expires").then(|| { let n = n.clone(); view! { <button class="link" style="font-size:13px;margin-left:18px" on:click=move |_| { let n = n.clone();
                                        leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/extend"), json!({"by": "24h"}), "Extended by 24 h").await }) }>"Extend 24 h"</button> } })}</div>
                                <div class="row" style="margin-bottom:12px"><span style="width:164px;display:flex;gap:10px;align-items:center"><Toggle on=Signal::derive(move || val("/paused") == true) set=move |v: bool| set("/paused", json!(v))/>"Paused"</span>
                                    <span class="small cap">"every service answers 503; state is kept"</span></div>
                                <div class="row"><span style="width:164px">"Reset"</span>
                                    <button class="link" style="font-size:13px" on:click=move |_| { let n = n_reset.clone(); leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/reset"), json!({}), "Reset to seeds").await }) }>"Reset all services to their seeds"</button></div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Telemetry and agents"</div>
                                <div class="row" style="margin-bottom:10px"><span style="width:164px">"OTLP"</span><span class="mono" style="width:300px">{otlp}</span>
                                    {if here { view! { <a class="link" style="font-size:13px" href="/telemetry/settings">"Exporter settings ›"</a> }.into_any() }
                                     else { let n = n.clone(); view! { <button class="link" style="font-size:13px" on:click=move |_| api::switch_env(&n, "/telemetry/settings")>"Exporter settings ›"</button> }.into_any() }}</div>
                                <div class="row" style="margin-bottom:6px"><span style="width:164px">"MCP endpoint"</span><span class="mono" style="width:300px">{mcp.clone()}</span>
                                    <button class="link" style="font-size:13px" on:click=move |_| ui::copy(&mcp, "MCP endpoint")>"Copy"</button></div>
                                <div class="row"><span style="width:164px"></span><span class="mono small cap" style="width:300px">{mcp2.clone()}</span>
                                    <button class="link" style="font-size:13px" on:click=move |_| ui::copy(&mcp2, "MCP endpoint")>"Copy"</button></div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:14px">"Danger"</div>
                                <div class="row"><button class="link danger" style="font-size:13px;width:164px;text-align:left" on:click=move |_| { typed.set(String::new()); del.set(true) }>{format!("Delete {}", label(&n))}</button>
                                    <span class="small cap">"everything, at once: its folder, both databases, identity keys, vault certificates, SMTP listeners and host names. Expiry deletes the same way."</span></div>
                            </div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                        }.into_any()
                    }}
                </div>                </div>
                <div class="side yaml" style="width:420px">
                    <div class="caps">{move || s(&saved.get(), "path")}</div>
                    <pre class="yaml envyaml">{move || {
                        let (sv, d) = (saved.get(), draft.get());
                        let changed: Vec<&str> = FIELDS.iter().filter(|(p, _)| sv.pointer(p) != d.pointer(p)).map(|(_, k)| *k).collect();
                        s(&sv, "file").lines().map(|l| {
                            let key = l.trim_start().split(':').next().unwrap_or("").to_string();
                            let on = changed.contains(&key.as_str());
                            view! { <div class="yl" class:mark=on>{l.to_string()}</div> }
                        }).collect_view()
                    }}</pre>
                    {move || (dirty.get() > 0).then(|| view! { <div class="small attn">"Marked lines change on Save. Editing the file by hand updates this form."</div> })}
                </div>
            </div>
            <Show when=move || del.get()>
                <div class="scrim" on:click=move |_| del.set(false)>
                    <div class="dialog" on:click=|e| e.stop_propagation()>
                        <h2 class="danger">{move || format!("Type {} to delete it", label(&name()))}</h2>
                        <div class="t2">"Services, state, scenarios and history go. This can't be undone."</div>
                        <input class="input mono" style="width:100%;margin-top:14px" placeholder=move || label(&name()) prop:value=move || typed.get() on:input=move |e| typed.set(event_target_value(&e))/>
                        <div class="actions">
                            <button class="btn" on:click=move |_| del.set(false)>"Cancel"</button>
                            <button class="btn danger solid" disabled=move || typed.get().trim().to_lowercase() != name() on:click=do_delete.clone()>{move || format!("Delete {}", label(&name()))}</button>
                        </div>
                    </div>
                </div>
            </Show>
        </div>
    }
}
