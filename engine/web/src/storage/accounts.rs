//! S5 · Services › Azure Blob (or S3) › Accounts: accounts on the left, the account's form (name,
//! endpoint, access and keys, containers, behaviour) drawn from the plugin's schema, and the live
//! YAML on the right with the changed lines marked. Save sends a patch with If-Match.

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{service_label, ServiceHead};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Toggle},
};

/// Paths the form edits (for the change count and the YAML marker).
const PATHS: [(&str, &str); 8] = [
    ("/settings/access/any_key", "any_key"),
    ("/settings/access/managed_identity", "managed_identity"),
    ("/settings/containers", "containers"),
    ("/settings/behaviour/latency", "latency"),
    ("/settings/behaviour/limit", "limit"),
    ("/settings/behaviour/versions", "versions"),
    ("/settings/behaviour/soft_delete", "soft_delete"),
    ("/settings/behaviour/seed_on_reset", "seed_on_reset"),
];

#[component]
pub fn Accounts() -> impl IntoView {
    let loc = use_location();
    let qm = use_query_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| loc.pathname.get().split('/').nth(2).unwrap_or("azure-blob").to_string());
    let list = RwSignal::new(Vec::<Value>::new());
    let sel = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let elsewhere = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let rotate = RwSignal::new(String::new());
    let rotate_open = RwSignal::new(false);
    let rm_cont = RwSignal::new(String::new());
    let rm_open = RwSignal::new(false);
    let adding = RwSignal::new(false);
    let new_cont = RwSignal::new((String::new(), "private".to_string()));
    let creating = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let shown_key = RwSignal::new((String::new(), String::new()));
    let dirty = Memo::new(move |_| { let (a, b) = (saved.get(), draft.get()); PATHS.iter().filter(|(p, _)| a.pointer(p) != b.pointer(p)).count() });
    let load = move |keep: bool| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            let Ok(v) = api::get(&format!("/services/{sv}/accounts")).await else { return };
            let accts = v["accounts"].as_array().cloned().unwrap_or_default();
            list.set(accts.clone());
            let want = if sel.get_untracked().is_empty() { qm.get_untracked().get("account").unwrap_or_default() } else { sel.get_untracked() };
            let a = accts.iter().find(|a| s(a, "account") == want).or(accts.first()).cloned();
            match a {
                Some(a) => {
                    let editing = keep && dirty.get_untracked() > 0;
                    if editing && a["etag"] != saved.get_untracked()["etag"] && s(&a, "account") == s(&saved.get_untracked(), "account") {
                        elsewhere.set(a);
                        return;
                    }
                    sel.set(s(&a, "account"));
                    if !editing { draft.set(a.clone()); }
                    saved.set(a);
                }
                None => { saved.set(Value::Null); draft.set(Value::Null); }
            }
        });
    };
    let want = Memo::new(move |_| qm.get().get("account").unwrap_or_default());
    Effect::new(move |_| { service.track(); want.track(); sel.set(String::new()); saved.set(Value::Null); load(false); creating.set(qm.get_untracked().get("new").is_some()); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "storage.account" { load(true); }
        t
    });
    let set = move |p: &'static str, v: Value| draft.update(|d| { if let Some(x) = d.pointer_mut(p) { *x = v; } });
    let val = move |p: &'static str| draft.get().pointer(p).cloned().unwrap_or(Value::Null);
    let save = move |force: bool| {
        let (sv, d, o) = (service.get_untracked(), draft.get_untracked(), saved.get_untracked());
        let mut patch = json!({});
        for (p, _) in PATHS {
            if o.pointer(p) != d.pointer(p) {
                let key = p.trim_start_matches("/settings/");
                let mut cur = &mut patch;
                let parts: Vec<&str> = key.split('/').collect();
                for (i, k) in parts.iter().enumerate() {
                    if i + 1 == parts.len() { cur[*k] = d.pointer(p).cloned().unwrap_or(Value::Null); } else { if cur[*k].is_null() { cur[*k] = json!({}); } cur = &mut cur[*k]; }
                }
            }
        }
        // Containers removed in the form go as null so the merge drops them.
        if let (Some(a), Some(b)) = (o["settings"]["containers"].as_object(), d["settings"]["containers"].as_object()) {
            for k in a.keys().filter(|k| !b.contains_key(*k)) { patch["containers"][k] = Value::Null; }
        }
        let mut body = json!({"patch": patch});
        if !force { body["if_match"] = o["etag"].clone(); }
        let acct = s(&d, "account");
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/services/{sv}/accounts/{}", enc(&acct)), body).await {
                Ok(v) => { saved.set(v.clone()); draft.set(v); elsewhere.set(Value::Null); err.set(String::new()); ui::toast("Saved"); }
                Err(e) if e.contains("changed since") => { load(true); }
                Err(e) => err.set(e),
            }
        });
    };
    let create = move |_| {
        let (sv, n) = (service.get_untracked(), new_name.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/services/{sv}/accounts"), json!({"account": n})).await {
                Ok(v) => { creating.set(false); new_name.set(String::new()); sel.set(s(&v, "account")); saved.set(v.clone()); draft.set(v); load(false); ui::toast("Account made with its keys"); }
                Err(e) => err.set(e),
            }
        });
    };
    let name_bad = move || { let n = new_name.get(); !n.is_empty() && !((3..=24).contains(&n.len()) && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')) };
    view! {
        <div class="page">
            {move || view! { <ServiceHead service=service.get() active="accounts" right=move || view! {
                {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> }) }}
                <button class="btn primary" disabled=move || dirty.get() == 0 on:click=move |_| save(false)>"Save"</button>
            }/> }}
            {move || (!elsewhere.get().is_null()).then(|| view! {
                <div class="strip">{format!("{} changed on disk. Someone (or an agent through the API) saved it. Your {} change(s) here are not saved yet.", s(&elsewhere.get(), "file"), dirty.get())}
                    <button class="link" on:click=move |_| save(true)>"Keep mine"</button>
                    <button class="link" on:click=move |_| { let v = elsewhere.get_untracked(); saved.set(v.clone()); draft.set(v); elsewhere.set(Value::Null); }>"Load the file"</button></div>
            })}
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">{move || if creating.get() { "New account" } else { "This account" }}</div>
                    {move || if creating.get() { view! {
                        <div style="padding:10px 24px">
                            <input class="input mono" style="width:100%" class:bad=name_bad placeholder="acme-docs-stg" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e).trim().to_string())/>
                            {move || name_bad().then(|| view! { <div class="err">"3 to 24 lowercase letters, numbers or dashes."</div> })}
                            <div class="row" style="margin-top:6px"><button class="link" disabled=move || new_name.get().is_empty() || name_bad() on:click=create>"Add"</button><button class="link plain" on:click=move |_| creating.set(false)>"Cancel"</button></div>
                        </div> }.into_any() } else { view! { <div class="small cap" style="padding:0 24px">"Keys, containers and behaviour of this account. Add another with + New account."</div> }.into_any() }}
                </div>
                <div class="pane pad form"><div style="max-width:560px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="callout"><div class="t">{format!("No {} accounts yet", service_label(&service.get()))}</div><div class="t2">"Add one: Stuntman makes its keys and an endpoint the app points at."</div></div> }.into_any(); }
                        let st = d["settings"].clone();
                        let s3 = service.get() == "s3";
                        let ep = s(&d, "endpoint");
                        let ep2 = ep.clone();
                        let conn = s(&d, "connection");
                        let acct = s(&d, "account");
                        view! {
                            <div class="field"><label>"Account name"</label>
                                <div class="row"><input class="input mono" style="width:300px" disabled prop:value=acct.clone()/><span class="small cap">"used in URLs and keys"</span></div></div>
                            <div class="row" style="margin-top:12px"><span style="width:84px">"Endpoint"</span><span class="mono grow">{ep}</span><button class="link" on:click=move |_| ui::copy(&ep2, "Endpoint")>"Copy"</button></div>
                            <div class="row" style="margin-top:4px"><span style="width:84px"></span><button class="link" on:click=move |_| ui::copy(&conn, if s3 { "S3 settings" } else { "Connection string" })>{if s3 { "Copy the S3 settings (endpoint, key, secret)" } else { "Copy connection string" }}</button></div>
                            <div class="caps" style="margin:22px 0 10px">"Access"</div>
                            <div class="row"><Toggle on=Signal::derive(move || val("/settings/access/any_key") == true) set=move |v: bool| set("/settings/access/any_key", json!(v))/>
                                <span>"Accept any key"</span><span class="small cap">"off: only the keys below; others get 403"</span></div>
                            {d["keys"].as_array().cloned().unwrap_or_default().into_iter().map(|k| {
                                let n = s(&k, "name");
                                let (n1, n2) = (n.clone(), n.clone());
                                view! { <div class="row" style="margin-top:10px"><span class="mono" style="width:150px">{n.clone()}</span>
                                    <span class="mono grow t2">{move || if shown_key.get().0 == n1 { shown_key.get().1 } else { s(&k, "masked") }}</span>
                                    <button class="link" on:click=move |_| { rotate.set(n2.clone()); rotate_open.set(true); }>"Rotate"</button></div> }
                            }).collect_view()}
                            {(!s3).then(|| view! { <div class="row" style="margin-top:12px"><Toggle on=Signal::derive(move || val("/settings/access/managed_identity") == true) set=move |v: bool| set("/settings/access/managed_identity", json!(v))/>
                                <span>"Accept managed identity tokens"</span><span class="small cap">"Bearer tokens (from Logins)"</span></div> })}
                            <div class="caps" style="margin:22px 0 10px">{if s3 { "Buckets" } else { "Containers" }}</div>
                            {st["containers"].as_object().cloned().unwrap_or_default().into_iter().map(|(n, c)| {
                                let n2 = n.clone();
                                let count = d["containers"].as_array().and_then(|x| x.iter().find(|y| s(y, "name") == n).map(|y| api::n(y, "objects"))).unwrap_or(0);
                                view! { <div class="row" style="margin-bottom:8px"><span class="mono" style="width:200px">{n.clone()}</span>
                                    <span class="grow t2">{match c["access"].as_str().unwrap_or("") { "blob" => "public read", "container" => "public read and list", "public-read" => "public read", _ => "private" }}</span>
                                    <span class="small cap" style="width:90px">{format!("{count} objects")}</span>
                                    <button class="link plain" on:click=move |_| { rm_cont.set(n2.clone()); rm_open.set(true); }>"Remove"</button></div> }
                            }).collect_view()}
                            {move || if adding.get() { view! {
                                <div class="row callout" style="max-width:520px">
                                    <input class="input mono" placeholder="invoices" prop:value=move || new_cont.get().0 on:input=move |e| new_cont.update(|c| c.0 = event_target_value(&e).trim().to_string())/>
                                    <select class="select" on:change=move |e| new_cont.update(|c| c.1 = event_target_value(&e))>
                                        {if service.get() == "s3" { vec![("private", "private"), ("public-read", "public read")] } else { vec![("private", "private"), ("blob", "public read"), ("container", "public read and list")] }.into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}</select>
                                    <button class="link" on:click=move |_| { let (n, a) = new_cont.get_untracked(); if !n.is_empty() { draft.update(|d| d["settings"]["containers"][&n] = json!({"access": a})); } adding.set(false); new_cont.set((String::new(), "private".into())); }>"Add"</button>
                                    <button class="link plain" on:click=move |_| adding.set(false)>"Cancel"</button></div> }.into_any()
                            } else { view! { <button class="link" style="font-size:13px" on:click=move |_| adding.set(true)>{if service.get() == "s3" { "+ Add bucket" } else { "+ Add container" }}</button> }.into_any() }}
                            <div class="caps" style="margin:22px 0 10px">"Behaviour"</div>
                            <div class="row" style="margin-bottom:10px"><span style="width:140px">"Latency"</span>
                                <select class="select" class:changed=move || saved.get().pointer("/settings/behaviour/latency") != draft.get().pointer("/settings/behaviour/latency") on:change=move |e| set("/settings/behaviour/latency", json!(event_target_value(&e)))>
                                    {[("none", "None"), ("same_region", "Same region · about 20 ms"), ("other_region", "Another region · about 120 ms"), ("slow_network", "Slow network · 300 ms ± 200")].into_iter().map(|(v, l)| { let on = s(&st["behaviour"], "latency") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                <span class="small cap">"added to every answer"</span></div>
                            <div class="row" style="margin-bottom:10px"><span style="width:140px">"Storage limit"</span>
                                <select class="select" class:changed=move || saved.get().pointer("/settings/behaviour/limit") != draft.get().pointer("/settings/behaviour/limit") on:change=move |e| set("/settings/behaviour/limit", json!(event_target_value(&e)))>
                                    {[("none", "No limit"), ("100MB", "100 MB"), ("1GB", "1 GB"), ("full", "Full now · every upload 507")].into_iter().map(|(v, l)| { let on = s(&st["behaviour"], "limit") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                <span class="small cap">"over it, uploads get 507"</span></div>
                            <div class="row" style="margin-bottom:10px"><Toggle on=Signal::derive(move || val("/settings/behaviour/versions") == true) set=move |v: bool| set("/settings/behaviour/versions", json!(v))/><span>"Keep versions"</span></div>
                            <div class="row" style="margin-bottom:10px"><Toggle on=Signal::derive(move || s(&val("/settings/behaviour"), "soft_delete") != "off") set=move |v: bool| set("/settings/behaviour/soft_delete", json!(if v { "7d" } else { "off" }))/><span>"Soft delete for 7 days"</span></div>
                            <div class="row"><Toggle on=Signal::derive(move || val("/settings/behaviour/seed_on_reset") == true) set=move |v: bool| set("/settings/behaviour/seed_on_reset", json!(v))/><span>{format!("Load {} on reset", s(&st, "seed"))}</span></div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:10px">{err.get()}</div> })}
                            <div class="small cap" style="margin-top:20px;border-top:1px solid var(--rule);padding-top:12px">{format!("Fields come from the {} plugin. The same file is the source of truth.", service_label(&service.get()))}</div>
                        }.into_any()
                    }}
                </div></div>
                <div class="side yaml" style="width:420px">
                    <div class="row"><span class="caps">{move || s(&saved.get(), "file")}</span><span class="small ok">"live"</span></div>
                    <pre class="yaml envyaml">{move || {
                        let (sv, d) = (saved.get(), draft.get());
                        let changed: Vec<&str> = PATHS.iter().filter(|(p, _)| sv.pointer(p) != d.pointer(p)).map(|(_, k)| *k).collect();
                        s(&sv, "yaml").lines().map(|l| {
                            let key = l.trim_start().split(':').next().unwrap_or("").to_string();
                            let on = changed.contains(&key.as_str());
                            view! { <div class="yl" class:mark=on>{l.to_string()}</div> }
                        }).collect_view()
                    }}</pre>
                    <div class="small cap">"The changed line is marked. Editing the file updates this form; editing the form updates the file on Save."</div>
                    {move || { let sv = saved.get(); (!sv.is_null()).then(|| view! { <a class="link" style="display:inline-block;margin-top:10px;font-size:13px" target="_blank" href=api::url(&format!("/api/services/{}/accounts/{}/yaml", service.get(), enc(&s(&sv, "account"))))>"Open the file ›"</a> }) }}
                </div>
            </div>
            <Confirm open=rotate_open question=Signal::derive(move || format!("Rotate {}?", rotate.get()))
                detail=Signal::derive(move || format!("Apps still signing with the old {} get 403 AuthenticationFailed. That is how you test a key rotation before doing it in production.", rotate.get()))
                action="Rotate" on_yes=move |_| {
                    let (sv, a, k) = (service.get_untracked(), s(&saved.get_untracked(), "account"), rotate.get_untracked());
                    leptos::task::spawn_local(async move {
                        match api::send("POST", &format!("/services/{sv}/accounts/{}/keys/{}/rotate", enc(&a), enc(&k)), json!({})).await {
                            Ok(v) => { shown_key.set((k.clone(), s(&v, "value"))); ui::copy(&s(&v, "value"), &format!("New {k}")); }
                            Err(e) => ui::toast_err(e),
                        }
                    });
                }/>
            <Confirm open=rm_open question=Signal::derive(move || { let n = rm_cont.get(); let c = draft.get()["containers"].as_array().and_then(|x| x.iter().find(|y| s(y, "name") == n).map(|y| api::n(y, "objects"))).unwrap_or(0); format!("Remove “{n}” and its {c} objects?") })
                detail=Signal::derive(|| "The objects are gone from this environment. The seed is not changed, so a reset brings them back.".to_string())
                action="Remove" on_yes=move |_| {
                    let (sv, a, c) = (service.get_untracked(), s(&saved.get_untracked(), "account"), rm_cont.get_untracked());
                    let _ = nav.clone();
                    leptos::task::spawn_local(async move {
                        match api::send("DELETE", &format!("/services/{sv}/accounts/{}/containers/{}", enc(&a), enc(&c)), json!({})).await {
                            Ok(v) => { ui::toast(format!("Removed with {} objects", api::n(&v, "objects"))); load(false); }
                            Err(e) => ui::toast_err(e),
                        }
                    });
                }/>
        </div>
    }
}
