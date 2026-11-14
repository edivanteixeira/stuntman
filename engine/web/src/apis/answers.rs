//! A4 · Services › <plugin> › Answers: named answers (status, body template, headers, sentence,
//! malformed on purpose), which route step uses each, forcing one for the next calls, and the
//! data tables the answers read.

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui::{self, Toggle},
};

const STATUSES: [(u16, &str); 22] = [
    (200, "200 OK"), (201, "201 Created"), (202, "202 Accepted"), (204, "204 No content"), (301, "301 Moved (with Location)"), (302, "302 Found (with Location)"), (308, "308 Permanent (with Location)"),
    (400, "400 Bad request"), (401, "401 Unauthorized"), (403, "403 Forbidden"), (404, "404 Not found"), (409, "409 Conflict"), (412, "412 Precondition failed"), (422, "422 Unprocessable"), (429, "429 Too many requests"),
    (500, "500 Internal error"), (502, "502 Bad gateway"), (503, "503 Unavailable"), (504, "504 Gateway timeout"), (418, "418 (any other code)"), (205, "205 Reset content"), (207, "207 Multi-status"),
];

#[component]
pub fn Answers() -> impl IntoView {
    let p = use_params_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let data = RwSignal::new(Value::Null);
    let tables = RwSignal::new(Value::Null);
    let forced = RwSignal::new(Value::Null);
    let forced_log = RwSignal::new(Vec::<Value>::new());
    let sel = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let force_count = RwSignal::new("3".to_string());
    let load = move || {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/plugins/{}/answers", enc(&sv))).await {
                let list = v["answers"].as_array().cloned().unwrap_or_default();
                let cur = list.iter().find(|a| s(&a["answer"], "name") == sel.get_untracked()).or(list.first()).cloned();
                data.set(v);
                if draft.get_untracked() == saved.get_untracked() {
                    if let Some(a) = cur { sel.set(s(&a["answer"], "name")); saved.set(a["answer"].clone()); draft.set(a["answer"].clone()); }
                }
            }
            if let Ok(v) = api::get(&format!("/plugins/{}/data", enc(&sv))).await { tables.set(v["tables"].clone()); }
            if let Ok(v) = api::get("/apis/services").await {
                let me = v["services"].as_array().cloned().unwrap_or_default().into_iter().find(|x| s(x, "name") == sv).unwrap_or(Value::Null);
                forced.set(me["forced"].clone());
                forced_log.set(me["forceLog"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    Effect::new(move |_| { service.track(); sel.set(String::new()); saved.set(Value::Null); draft.set(Value::Null); load(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("apis.") { load(); }
        t
    });
    let dirty = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        if b.is_null() { 0 } else if a.is_null() { 1 } else { ["/name", "/status", "/body", "/headers", "/raw", "/sentence"].iter().filter(|p| a.pointer(p) != b.pointer(p)).count() }
    });
    let save = move |_| {
        let (sv, d, o) = (service.get_untracked(), draft.get_untracked(), saved.get_untracked());
        let name = s(&d, "name");
        let old = if o.is_null() { name.clone() } else { s(&o, "name") };
        let body = json!({"old_name": old, "status": d["status"], "body": s(&d, "body"), "headers": d["headers"].clone().as_object().cloned().unwrap_or_default(), "raw": d["raw"] == true, "sentence": s(&d, "sentence")});
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/answers/{}", enc(&sv), enc(&name)), body).await {
                Ok(v) => { err.set(String::new()); saved.set(v["answer"].clone()); draft.set(v["answer"].clone()); sel.set(name); ui::toast("Saved"); load(); }
                Err(e) => err.set(e),
            }
        });
    };
    let force = move |on: bool| {
        let (sv, a, n) = (service.get_untracked(), s(&draft.get_untracked(), "name"), force_count.get_untracked());
        leptos::task::spawn_local(async move {
            let body = if !on { json!({"answer": "default"}) } else if n.ends_with('m') || n.ends_with('h') { json!({"answer": a, "for": n}) } else { json!({"answer": a, "count": n.parse::<u64>().unwrap_or(1)}) };
            match api::send("POST", &format!("/apis/{}/force", enc(&sv)), body).await { Ok(_) => { ui::toast(if on { "Forced; it ends by itself" } else { "Back to the data" }); load(); } Err(e) => ui::toast_err(e) }
        });
    };
    let add_row = move |table: String, row: Value| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/data/{}", enc(&sv), enc(&table)), json!({"rows": [row], "add": true})).await { Ok(_) => { ui::toast("Added"); load(); } Err(e) => ui::toast_err(e) }
        });
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="answers" right=move || view! {
                {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| { draft.set(saved.get_untracked()); err.set(String::new()); }>"Discard"</button> }) }}
                <button class="btn primary" disabled=move || dirty.get() == 0 on:click=save>"Save"</button>
            }/> }}
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">"Named answers"</div>
                    {move || data.get()["answers"].as_array().cloned().unwrap_or_default().into_iter().map(|a| {
                        let x = a["answer"].clone();
                        let n = s(&x, "name");
                        let (n1, x2) = (n.clone(), x.clone());
                        let st = api::n(&x, "status");
                        let unused = a["usedBy"].as_array().map(|u| u.is_empty()).unwrap_or(true);
                        view! { <div class="crow" class:on=move || sel.get() == n1 on:click=move |_| { if dirty.get_untracked() == 0 { sel.set(s(&x2, "name")); saved.set(x2.clone()); draft.set(x2.clone()); } else { ui::toast_err("Save or discard the change first") } }>
                            <span class:cap=unused>{n.replace('_', " ")}</span><span class="mono small" class:ok={st < 300} class:attn={(300..500).contains(&st) && st != 401} class:danger={st >= 400}>{st}</span></div> }
                    }).collect_view()}
                    <button class="link add" on:click=move |_| { sel.set("\u{0}new".into()); saved.set(Value::Null); draft.set(json!({"name": "new_answer", "status": 200, "body": "{\n  \"ok\": true\n}", "sentence": ""})); }>"+ New answer"</button>
                    <div class="small cap" style="padding:8px 24px">"Slow is not an answer: set latency on the route or in Auth and limits."</div>
                </div>
                <div class="pane pad form"><div style="max-width:560px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let used = data.get()["answers"].as_array().cloned().unwrap_or_default().into_iter().find(|a| s(&a["answer"], "name") == sel.get()).map(|a| a["usedBy"].clone()).unwrap_or(json!([]));
                        let st = api::n(&d, "status");
                        let sv = service.get();
                        view! {
                            <input class="input" style="font-size:16px;font-weight:600;width:320px" prop:value=s(&d, "name") on:change=move |e| draft.update(|x| x["name"] = json!(event_target_value(&e).trim().replace(' ', "_")))/>
                            <div class="small cap" style="margin:4px 0 14px">{format!("{st}  ·  {}", if s(&d, "note").is_empty() { "a named answer".to_string() } else { s(&d, "note") })}</div>
                            <div class="row"><span style="width:80px">"Status"</span>
                                <select class="select" on:change=move |e| draft.update(|x| x["status"] = json!(event_target_value(&e).parse::<u64>().unwrap_or(200)))>
                                    {STATUSES.iter().map(|(c, l)| { let on = *c as i64 == st; view! { <option value=c.to_string() selected=on>{*l}</option> } }).collect_view()}</select>
                                <label class="check" style="margin-left:14px"><input type="checkbox" prop:checked=d["raw"] == true on:change=move |e| draft.update(|x| x["raw"] = json!(event_target_checked(&e)))/>"Malformed on purpose (raw, not checked)"</label></div>
                            <div class="field" style="margin-top:14px"><label>"Body"</label>
                                <textarea class="yaml" style="min-height:220px" prop:value=s(&d, "body") on:input=move |e| draft.update(|x| x["body"] = json!(event_target_value(&e)))></textarea>
                                <div class="small cap">"{{ … }} fills from the request, the lets and the data. Checked against the route's schema unless malformed on purpose."</div></div>
                            <div class="field" style="margin-top:12px"><label>"Headers"</label>
                                <input class="input mono" placeholder="Location: /v2/x, Retry-After: 30" prop:value=d["headers"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ")).unwrap_or_default()
                                    on:change=move |e| { let t = event_target_value(&e); draft.update(|x| x["headers"] = json!(t.split(',').filter_map(|kv| kv.split_once(':').map(|(k, v)| (k.trim().to_string(), json!(v.trim())))).collect::<serde_json::Map<_, _>>())); }/></div>
                            <div class="field" style="margin-top:12px"><label>"Sentence on Calls"</label>
                                <input class="input" placeholder="Validated {{ body.cpf | mask }}…" prop:value=s(&d, "sentence") on:change=move |e| draft.update(|x| x["sentence"] = json!(event_target_value(&e)))/></div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="callout danger-l"><div class="t danger">{err.get()}</div><div class="t2">"Checked against the provider's schema while you edit. To test a broken provider, mark it malformed on purpose."</div></div> })}
                            <div class="caps" style="margin:22px 0 8px">"Used by"</div>
                            {if used.as_array().map(|u| u.is_empty()).unwrap_or(true) {
                                view! { <div class="small cap">{format!("“{}” is used by no route: it exists only to be forced. Add a rule in a route to reach it from the data.", s(&d, "name"))}</div> }.into_any()
                            } else {
                                used.as_array().cloned().unwrap_or_default().into_iter().map(|u| view! { <a class="link" style="display:block;font-size:13px" href=format!("/apis/{}/routes?route={}", enc(&sv), enc(&s(&u, "route")))>
                                    {format!("{}  ·  step {}{}", s(&u, "route"), s(&u, "step").to_uppercase(), if u["when"].is_null() { String::new() } else { format!("  ·  “{}”", s(&u, "when")) })}</a> }).collect_view().into_any()
                            }}
                        }.into_any()
                    }}
                </div></div>
                <div class="side envside">
                    <div class="caps">"Force for the next calls"</div>
                    {move || {
                        let f = forced.get();
                        let name = s(&draft.get(), "name");
                        let on = s(&f, "answer") == name && !name.is_empty();
                        view! {
                            <div class="row" style="margin:10px 0"><Toggle on=on set=move |v: bool| force(v)/><span>{format!("Answer {} to every call", name.replace('_', " "))}</span></div>
                            <div class="row"><span style="width:30px">"for"</span>
                                <select class="select" on:change=move |e| force_count.set(event_target_value(&e))>
                                    {[("1", "the next call"), ("3", "the next 3 calls"), ("10", "the next 10 calls"), ("10m", "10 minutes"), ("1h", "1 hour")].into_iter().map(|(v, l)| view! { <option value=v selected=move || force_count.get() == v>{l}</option> }).collect_view()}</select></div>
                            {(!f.is_null()).then(|| view! { <div class="small attn" style="margin-top:8px">{format!("{} forced{}  ·  by {}", s(&f, "answer").replace('_', " "), if f["left"].is_number() { format!(" for the next {} calls", api::n(&f, "left")) } else { format!(" until {}", api::hm(api::n(&f, "until"))) }, s(&f, "by").trim_start_matches("api:").trim_start_matches("mcp:"))}</div> })}
                            <div class="small cap" style="margin-top:6px">"Ends by itself."</div>
                        }
                    }}
                    <div class="caps" style="margin:20px 0 8px">"Who forced what"</div>
                    {move || { let l = forced_log.get(); if l.is_empty() { view! { <div class="small cap">"Nobody forced an answer yet."</div> }.into_any() } else {
                        l.into_iter().take(6).map(|x| view! { <div class="hrow"><span class="mono small cap">{api::hm(api::n(&x, "at"))}</span>
                            <div class="small">{format!("{} {}{}", s(&x, "by").trim_start_matches("api:").trim_start_matches("mcp:"), if s(&x, "answer") == "default" { "stopped forcing".to_string() } else { format!("forced {}", s(&x, "answer").replace('_', " ")) },
                                if x["count"].is_number() { format!(" × {}", api::n(&x, "count")) } else if x["until"].is_number() { format!(" until {}", api::hm(api::n(&x, "until"))) } else { String::new() })}</div></div> }).collect_view().into_any() } }}
                    <div class="caps" style="margin:24px 0 10px">"Data the answers read"</div>
                    {move || tables.get().as_object().cloned().unwrap_or_default().into_iter().map(|(t, rows)| {
                        let rows = rows.as_array().cloned().unwrap_or_default();
                        let cols: Vec<String> = rows.first().and_then(|r| r.as_object()).map(|o| o.keys().filter(|k| *k != "base64" && *k != "sha256").cloned().collect()).unwrap_or_default();
                        let t2 = t.clone();
                        let add = add_row.clone();
                        let is_cpf = cols.iter().any(|c| c == "cpf");
                        let cols2 = cols.clone();
                        view! {
                            <div class="row"><span class="grow">{t.clone()}</span><span class="small cap">{format!("{} entries", rows.len())}</span></div>
                            <table class="kv2" style="margin:4px 0 6px 12px">{rows.iter().take(8).map(|r| view! { <tr>{cols.iter().map(|c| view! { <td class="mono">{s(r, c)}</td> }).collect_view()}</tr> }).collect_view()}</table>
                            {(rows.len() > 8).then(|| view! { <div class="small cap" style="margin-left:12px">{format!("… and {} more", rows.len() - 8)}</div> })}
                            <button class="link" style="margin:0 0 14px 12px;font-size:13px" on:click=move |_| {
                                let (t, add, cols) = (t2.clone(), add.clone(), cols2.clone());
                                leptos::task::spawn_local(async move {
                                    let mut row = serde_json::Map::new();
                                    if is_cpf { if let Ok(c) = api::get("/apis/synthetic-cpf").await { row.insert("cpf".into(), c["formatted"].clone()); } }
                                    for c in cols.iter().filter(|c| *c != "cpf") { row.insert(c.clone(), json!(if c == "situation" { "suspended" } else { "new" })); }
                                    add(t, Value::Object(row));
                                });
                            }>{if is_cpf { "+ Add a synthetic CPF" } else { "+ Add a row" }}</button>
                        }
                    }).collect_view()}
                    <div class="small cap">"CPFs are synthetic and pass the check digit; never use real ones."</div>
                </div>
            </div>
        </div>
    }
}
