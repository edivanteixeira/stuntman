//! E4 · Services › E-mail › Extractors: built-ins and yours, the rule editor, and Try it against one
//! message and the last 50, with the reason for every miss and a suggestion when one is obvious.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use serde_json::{json, Value};

use super::ServicesHead;
use crate::{
    api::{self, s},
    ui::{self, Confirm, Dialog, Pop, Toggle},
};

const SOURCES: [(&str, &str, &str); 6] = [
    ("subject", "Subject", "Subject matches  (regex)"),
    ("text", "Text", "Text matches  (regex)"),
    ("html", "HTML", "CSS selector  ·  takes the element's text"),
    ("link", "Link", "Link whose URL matches  (regex)"),
    ("header", "Header", "Header name"),
    ("attachment", "Attachment", "File name matches  (regex)"),
];

fn blank() -> Value {
    json!({"name": "", "from": "text", "rule": "", "onlyOn": {"when": "any", "value": ""}, "followRedirects": false, "allMatches": false, "strip": false})
}

fn ordinal(n: usize) -> String {
    match n {
        1 => "1st".into(),
        2 => "2nd".into(),
        3 => "3rd".into(),
        n => format!("{n}th"),
    }
}

#[component]
pub fn Extractors() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let data = RwSignal::new(Value::Null);
    let selected = Memo::new(move |_| params.get().get("name").unwrap_or_default());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let code_saved = RwSignal::new(Value::Null);
    let code_draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let messages = RwSignal::new(Vec::<Value>::new());
    let sample = RwSignal::new(String::new());
    let one = RwSignal::new(Value::Null);
    let many = RwSignal::new(Value::Null);
    let suggestion = RwSignal::new(None::<String>);
    let yaml_open = RwSignal::new(false);
    let yaml = RwSignal::new(String::new());
    let menu = RwSignal::new(false);
    let confirm = RwSignal::new(false);

    let load = move || {
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/channels/email/extractors").await {
                code_saved.set(v["code"].clone());
                if code_draft.get_untracked().is_null() || code_draft.get_untracked() == code_saved.get_untracked() {
                    code_draft.set(v["code"].clone());
                }
                data.set(v);
            }
            if let Ok(v) = api::get("/messages?limit=50").await {
                let m = v["messages"].as_array().cloned().unwrap_or_default();
                if sample.get_untracked().is_empty() {
                    let from_q = query.get_untracked().get("msg").unwrap_or_default();
                    sample.set(if from_q.is_empty() { m.first().map(|x| s(x, "id")).unwrap_or_default() } else { from_q });
                }
                messages.set(m);
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && (topic == "email.extractor" || (topic == "email.message" && s(&live.last.get_untracked(), "what") == "created")) {
            load();
        }
        t
    });
    // Pick what to edit: one of yours, a built-in, or a new one (possibly prefilled from E1c).
    Effect::new(move |_| {
        let name = selected.get();
        let d = data.get();
        if d.is_null() {
            return;
        }
        err.set(String::new());
        if name.is_empty() || name == "new" {
            let q = query.get_untracked();
            let mut b = blank();
            if let Some(f) = q.get("from") {
                b["from"] = json!(f);
            }
            if let Some(r) = q.get("rule") {
                b["rule"] = json!(r);
            }
            if q.get("strip").as_deref() == Some("true") {
                b["strip"] = json!(true);
            }
            if name.is_empty() && q.get("new").is_none() {
                if let Some(first) = d["yours"].as_array().and_then(|a| a.first()) {
                    let e = first["extractor"].clone();
                    saved.set(e.clone());
                    draft.set(e);
                    return;
                }
            }
            saved.set(Value::Null);
            draft.set(b);
            return;
        }
        if let Some(e) = d["yours"].as_array().and_then(|a| a.iter().find(|e| s(&e["extractor"], "name") == name)) {
            if saved.get_untracked() != e["extractor"] || draft.get_untracked().is_null() {
                saved.set(e["extractor"].clone());
                draft.set(e["extractor"].clone());
            }
        } else {
            saved.set(json!({"builtin": name}));
            draft.set(json!({"builtin": name}));
        }
    });

    let is_builtin = move || draft.get()["builtin"].is_string();
    let dirty = Memo::new(move |_| {
        if draft.get()["builtin"].as_str() == Some("code") {
            let (a, b) = (code_saved.get(), code_draft.get());
            return ["on", "minDigits", "maxDigits", "words", "preferProminent"].iter().filter(|k| a[**k] != b[**k]).count();
        }
        if draft.get()["builtin"].is_string() {
            return 0;
        }
        let (a, b) = (saved.get(), draft.get());
        if a.is_null() {
            return ["name", "rule"].iter().filter(|k| !s(&b, k).is_empty()).count().max(1);
        }
        ["name", "from", "rule", "onlyOn", "followRedirects", "allMatches", "strip"].iter().filter(|k| a[**k] != b[**k]).count()
    });

    // Try it: live, nothing saved.
    Effect::new(move |_| {
        let d = draft.get();
        let msg = sample.get();
        if d["builtin"].is_string() || s(&d, "rule").is_empty() {
            one.set(Value::Null);
            many.set(Value::Null);
            suggestion.set(None);
            return;
        }
        let body = |extra: Value| {
            let mut b = d.clone();
            if let (Some(o), Some(x)) = (b.as_object_mut(), extra.as_object()) {
                o.extend(x.clone());
            }
            b
        };
        let b1 = body(json!({"messageId": msg}));
        let b2 = body(json!({"last": 50}));
        let b3 = body(json!({"last": 50, "onlyOn": {"when": "any"}}));
        let when = s(&d["onlyOn"], "when");
        let pattern = s(&d["onlyOn"], "value");
        leptos::task::spawn_local(async move {
            if !msg.is_empty() {
                if let Ok(v) = api::send("POST", "/channels/email/extractors/test", b1).await {
                    one.set(v);
                }
            }
            if let Ok(v) = api::send("POST", "/channels/email/extractors/test", b2).await {
                // When "only on" skipped messages the rule itself would have matched, suggest widening it.
                let mut sug = None;
                if when == "subject" && v["skipped"].as_i64().unwrap_or(0) > 0 {
                    if let Ok(w) = api::send("POST", "/channels/email/extractors/test", b3).await {
                        let skipped: Vec<String> = v["results"].as_array().map(|a| a.iter().filter(|r| r["outcome"]["skipped"] == true).map(|r| s(r, "id")).collect()).unwrap_or_default();
                        sug = w["results"].as_array().and_then(|a| {
                            a.iter().find(|r| skipped.contains(&s(r, "id")) && !r["outcome"]["value"].is_null()).map(|r| s(r, "subject").split(':').next().unwrap_or("").trim().to_string())
                        }).filter(|p| !p.is_empty() && !pattern.contains(p.as_str()));
                    }
                }
                suggestion.set(sug);
                many.set(v);
            }
        });
    });

    let nav_save = nav.clone();
    let save = move |_| {
        let d = draft.get_untracked();
        if d["builtin"].as_str() == Some("code") {
            let c = code_draft.get_untracked();
            leptos::task::spawn_local(async move {
                match api::send("PATCH", "/channels/email/extractors/builtin/code", c).await {
                    Ok(v) => { code_saved.set(v.clone()); code_draft.set(v); ui::toast("Saved"); }
                    Err(e) => err.set(e),
                }
            });
            return;
        }
        let old = saved.get_untracked();
        let nav = nav_save.clone();
        leptos::task::spawn_local(async move {
            let r = if old.is_null() {
                api::send("POST", "/channels/email/extractors", d.clone()).await
            } else {
                let mut b = d.clone();
                b["newName"] = b["name"].clone();
                api::send("PATCH", &format!("/channels/email/extractors/{}", s(&old, "name")), b).await
            };
            match r {
                Ok(v) => {
                    saved.set(v.clone());
                    draft.set(v.clone());
                    err.set(String::new());
                    ui::toast(format!("Saved in email/extractors.yaml"));
                    nav(&format!("/services/email/extractors/{}", s(&v, "name")), Default::default());
                }
                Err(e) => err.set(e),
            }
        });
    };
    let nav_del = nav.clone();
    let delete = move |_| {
        let n = s(&saved.get_untracked(), "name");
        let nav = nav_del.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/channels/email/extractors/{n}"), json!({})).await {
                Ok(_) => { ui::toast(format!("{n} deleted")); nav("/services/email/extractors", Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let header_right = move || view! {
        {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
            <button class="btn" on:click=move |_| { draft.set(if saved.get_untracked().is_null() { blank() } else { saved.get_untracked() }); code_draft.set(code_saved.get_untracked()); }>"Discard"</button> }) }}
        <button class="btn primary" disabled=move || dirty.get() == 0 || is_builtin() && draft.get()["builtin"] != "code" on:click=save.clone()>"Save"</button>
    };
    let set = move |k: &'static str, v: Value| draft.update(|d| d[k] = v);
    let rule_err = move || one.get()["error"].as_str().map(String::from).or(many.get()["error"].as_str().map(String::from));
    let nav_list = nav.clone();

    view! {
        <div class="page">
            <ServicesHead active="extractors" right=header_right/>
            <div class="body">
                <div class="list narrow">
                    <div class="section caps" style="padding-top:16px">"Built in  ·  no setup"</div>
                    {let nav_list = nav_list.clone(); move || data.get()["builtin"].as_array().cloned().unwrap_or_default().into_iter().map(|b| {
                        let n = s(&b, "name");
                        let (n2, n3) = (n.clone(), n.clone());
                        let nav = nav_list.clone();
                        view! { <div class="srow" class:on=move || selected.get() == n2 on:click=move |_| nav(&format!("/services/email/extractors/{n3}"), Default::default())>
                            <div class="n">{n}</div><div class="d">{s(&b, "description")}</div></div> }
                    }).collect_view()}
                    <div class="section caps">"Yours"</div>
                    {let nav_list = nav_list.clone(); move || data.get()["yours"].as_array().cloned().unwrap_or_default().into_iter().map(|e| {
                        let x = &e["extractor"];
                        let n = s(x, "name");
                        let (n2, n3) = (n.clone(), n.clone());
                        let nav = nav_list.clone();
                        let today = api::n(&e, "matchedToday");
                        let last = e["lastMatch"].as_i64();
                        let stale = last.map(|l| api::now() as i64 - l > 86_400_000).unwrap_or(true);
                        let edited = selected.get() == n && dirty.get() > 0;
                        let health = if today > 0 { format!("matched {today} today") } else if let Some(l) = last { format!("no match in {}", api::span(api::now() as i64 - l)) } else { "no match yet".to_string() };
                        view! { <div class="srow" class:on=move || selected.get() == n2 || (selected.get().is_empty() && saved.get()["name"] == n2.as_str()) on:click=move |_| nav(&format!("/services/email/extractors/{n3}"), Default::default())>
                            <div class="n">{n}</div>
                            <div class="d" class:danger=stale && today == 0 class:attn=edited>{format!("{}  ·  {health}{}", s(x, "from"), if edited { "  ·  edited" } else { "" })}</div></div> }
                    }).collect_view()}
                    <div class="srow t2" on:click={ let nav = nav_list.clone(); move |_| nav("/services/email/extractors/new", Default::default()) }>"+  New extractor"</div>
                    <div class="note">"Each channel has its own extractors, with the sources that channel has. The same name in two channels is one field for tests: extract=\"otp\" works for e-mail and SMS."</div>
                </div>

                <div class="pane pad form">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        match d["builtin"].as_str() {
                            Some("code") => view! { <CodeEditor draft=code_draft/> }.into_any(),
                            Some(b) => view! {
                                <h2 class="mono" style="margin:0 0 8px">{b.to_string()}</h2>
                                <div class="t2">{if b == "links" { "Every link in the text and HTML, in order, without duplicates. Extractors with “Follow redirects” resolve short links to the final URL." } else { "Name, content type, size and SHA-256 of every attachment that is not inline. Download them from the message or with get_attachment()." }}</div>
                                <div class="small cap" style="margin-top:12px">"Built-ins are on for every e-mail and cannot be deleted."</div>
                            }.into_any(),
                            None => {
                                let src = s(&d, "from");
                                let label = SOURCES.iter().find(|x| x.0 == src).map(|x| x.2).unwrap_or("Rule");
                                let used = data.get()["yours"].as_array().and_then(|a| a.iter().find(|e| s(&e["extractor"], "name") == s(&d, "name")).map(|e| e["usedBy"].as_array().cloned().unwrap_or_default())).unwrap_or_default();
                                let saved_v = saved.get();
                                let rule_changed = !saved_v.is_null() && saved_v["rule"] != d["rule"];
                                let when = s(&d["onlyOn"], "when");
                                view! {
                                    <div class="field"><label>"Name"</label>
                                        <div class="row">
                                            <input class="input mono" style="width:220px" placeholder="name, e.g. resetLink" prop:value=s(&d, "name") on:change=move |e| set("name", json!(event_target_value(&e)))/>
                                            <span class="small cap">{if used.is_empty() { if saved_v.is_null() { "Not saved yet".to_string() } else { "Not used by any inbox step".to_string() } } else { format!("Used by {}", used.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")) }}</span>
                                            <span class="grow"></span>
                                            {(!saved_v.is_null()).then(|| view! {
                                                <div class="anchor"><button class="btn icon" on:click=move |_| menu.set(true)>"⋯"</button>
                                                    <Pop open=menu right=true>
                                                        <div class="mi" on:click=move |_| { let mut c = draft.get_untracked(); c["name"] = json!(format!("{}Copy", s(&c, "name"))); saved.set(Value::Null); draft.set(c); }>"Duplicate"</div>
                                                        {["sms", "whatsapp"].into_iter().map(|ch| view! { <div class="mi" on:click=move |_| {
                                                            let n = s(&saved.get_untracked(), "name");
                                                            leptos::task::spawn_local(async move { if let Err(e) = api::send("POST", &format!("/channels/email/extractors/{n}/copy"), json!({"toChannel": ch})).await { ui::toast_err(e) } });
                                                        }>{if ch == "sms" { "Copy to SMS" } else { "Copy to WhatsApp" }}</div> }).collect_view()}
                                                        <div class="rule"></div>
                                                        <div class="mi danger" on:click=move |_| confirm.set(true)>"Delete extractor…"</div>
                                                    </Pop></div>
                                            })}
                                        </div>
                                    </div>
                                    <div class="field" style="margin-top:16px"><label>"Only on messages where"</label>
                                        <div class="row">
                                            <select class="select" style="width:170px" on:change=move |e| draft.update(|d| d["onlyOn"]["when"] = json!(event_target_value(&e)))>
                                                {[("any", "any message"), ("subject", "subject matches"), ("kind", "kind is"), ("from", "sent from"), ("to", "sent to an inbox")].into_iter().map(|(v, l)| view! { <option value=v selected=when == v>{l}</option> }).collect_view()}
                                            </select>
                                            {(when != "any").then(|| view! { <input class="input mono grow" prop:value=s(&d["onlyOn"], "value") on:change=move |e| draft.update(|d| d["onlyOn"]["value"] = json!(event_target_value(&e)))/> })}
                                        </div></div>
                                    <div class="field" style="margin-top:16px"><label>"Take it from"</label>
                                        <div class="sources">{SOURCES.iter().map(|(k, l, _)| { let on = src == *k; view! { <button class:on=on on:click=move |_| set("from", json!(k))>{*l}</button> } }).collect_view()}</div></div>
                                    <div class="field" style="margin-top:16px"><label>{label}</label>
                                        <input class="input mono" class:changed=rule_changed class:bad=move || rule_err().is_some() prop:value=s(&d, "rule") on:input=move |e| set("rule", json!(event_target_value(&e)))/>
                                        {move || match rule_err() {
                                            Some(e) => view! { <div class="err">{format!("{e}. Nothing is saved until the rule is valid.")}</div> }.into_any(),
                                            None => view! { <div class="small cap">"Regex, CSS selector or header name. A capture group becomes the value."</div> }.into_any(),
                                        }}</div>
                                    <div class="col" style="gap:12px;margin-top:18px">
                                        {(src == "link").then(|| view! { <div class="row"><Toggle on=Signal::derive(move || draft.get()["followRedirects"] == true) set=move |v: bool| set("followRedirects", json!(v))/><span>"Follow redirects to the final URL"</span><span class="t2">"·  for short links"</span></div> })}
                                        <div class="row"><Toggle on=Signal::derive(move || draft.get()["allMatches"] == true) set=move |v: bool| set("allMatches", json!(v))/><span>"Keep every match as a list"</span><span class="t2">"·  off: the first match wins"</span></div>
                                        <div class="row"><Toggle on=Signal::derive(move || draft.get()["strip"] == true) set=move |v: bool| set("strip", json!(v))/><span>"Remove spaces and dashes"</span><span class="t2">"·  “482 913” → “482913”"</span></div>
                                    </div>
                                    {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:12px">{err.get()}</div> })}
                                    <div class="small cap" style="margin-top:24px;padding-top:14px;border-top:1px solid var(--rule)">
                                        {move || format!("Saved in email/extractors.yaml for {}  ·  ", api::s(&env.get(), "env").to_uppercase())}
                                        <button class="link plain" on:click=move |_| { leptos::task::spawn_local(async move { if let Ok(t) = api::text("/api/channels/email/extractors.yaml").await { yaml.set(t); yaml_open.set(true); } }); }>"View YAML"</button>
                                    </div>
                                }.into_any()
                            }
                        }
                    }}
                </div>

                <div class="side">
                    <div class="caps">"Try it"</div>
                    <select class="select" style="width:100%;margin-top:10px" on:change=move |e| sample.set(event_target_value(&e))>
                        {move || messages.get().into_iter().map(|m| { let id = s(&m, "id"); let (i1, i2) = (id.clone(), id.clone()); view! {
                            <option value=i1 selected=move || sample.get() == i2>{format!("#{:04}  {}", api::n(&m, "seq"), s(&m, "subject"))}</option> } }).collect_view()}
                    </select>
                    {move || {
                        if is_builtin() { return view! { <div class="small cap" style="margin-top:12px">"Built-ins run on every message; their values show in the message pane."</div> }.into_any(); }
                        let o = one.get();
                        let d = draft.get();
                        if s(&d, "rule").is_empty() { return view! { <div class="small cap" style="margin-top:12px">"Write a rule: Try it runs it on this message and the last 50, and saves nothing."</div> }.into_any(); }
                        let r = o["results"][0].clone();
                        let out = r["outcome"].clone();
                        let links = r["links"].as_array().cloned().unwrap_or_default();
                        let hit = out["linkIndex"].as_i64();
                        let name = if s(&d, "name").is_empty() { "value".to_string() } else { s(&d, "name") };
                        view! {
                            {(s(&d, "from") == "link").then(|| view! {
                                <div style="font-weight:600;margin:16px 0 8px">"Links in this message"</div>
                                {links.iter().enumerate().map(|(i, l)| view! {
                                    <div class="linkbox" class:hit=hit == Some(i as i64)>{s(l, "url")}{(!s(l, "finalUrl").is_empty()).then(|| view! { <div>{format!("→ {}", s(l, "finalUrl"))}</div> })}</div>
                                }).collect_view()}
                                {links.is_empty().then(|| view! { <div class="small cap">"No links in this message."</div> })}
                            })}
                            <div style="font-weight:600;margin:16px 0 6px">"Result"</div>
                            {if out["skipped"] == true {
                                view! { <div class="small t2">"Skipped: this message does not match “Only on”."</div> }.into_any()
                            } else if !out["value"].is_null() {
                                let v = out["value"].as_str().map(String::from).unwrap_or(out["value"].to_string());
                                let redirects = links.get(hit.unwrap_or(0) as usize).map(|l| api::n(l, "redirects")).unwrap_or(0);
                                view! {
                                    <div class="mono s12" style="word-break:break-all">{format!("{name} = {v}")}</div>
                                    <div class="small ok" style="margin-top:6px">{format!("✓ matched{}{}", hit.map(|h| format!(" the {} link", ordinal(h as usize + 1))).unwrap_or_default(),
                                        if redirects > 0 && d["followRedirects"] == true { format!("  ·  followed {redirects} redirect{}", if redirects == 1 { "" } else { "s" }) } else { String::new() })}
                                        {if d["strip"] == true { "  ·  spaces removed" } else { "" }}</div>
                                }.into_any()
                            } else if o.is_null() {
                                view! { <div class="small cap">"…"</div> }.into_any()
                            } else {
                                view! { <div class="small danger">{format!("✕ {} in #{:04}.{}", match s(&out, "why").as_str() { "" => "No match".to_string(), w => { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() } }, api::n(&r, "seq"),
                                    if s(&out, "nearest").is_empty() { String::new() } else { format!(" Nearest candidate: “{}”.", s(&out, "nearest")) })}</div> }.into_any()
                            }}
                            {move || {
                                let m = many.get();
                                if m.is_null() || m["error"].is_string() { return None; }
                                let (a, b, c) = (api::n(&m, "matched"), api::n(&m, "skipped"), api::n(&m, "missed"));
                                let tot = (a + b + c).max(1) as f64;
                                let misses: Vec<Value> = m["results"].as_array().map(|x| x.iter().filter(|r| r["outcome"]["skipped"] != true && r["outcome"]["value"].is_null()).cloned().collect()).unwrap_or_default();
                                let src = s(&draft.get(), "from");
                                Some(view! {
                                    <div style="border-top:1px solid var(--rule);margin-top:18px;padding-top:14px;font-weight:600">{move || format!("Against the last {} e-mails in {}", a + b + c, api::s(&env.get(), "env").to_uppercase())}</div>
                                    <div class="bar"><div class="m" style=format!("width:{}%", a as f64 * 100.0 / tot)></div><div class="s" style=format!("width:{}%", b as f64 * 100.0 / tot)></div><div class="x" style=format!("width:{}%", c as f64 * 100.0 / tot)></div></div>
                                    <div class="small t2">{format!("{a} matched   ·   {b} skipped by “Only on”   ·   {c} had no {}", if src == "link" { "such link" } else { "match" })}</div>
                                    {(!misses.is_empty()).then(|| view! {
                                        <div style="font-weight:600;margin:14px 0 6px">{if src == "link" { "No link found in" } else { "No match in" }}</div>
                                        {misses.into_iter().take(6).map(|r| { let id = s(&r, "id"); view! {
                                            <div class="small danger" style="cursor:pointer" on:click=move |_| sample.set(id.clone())>{format!("#{:04}  {}  {}", api::n(&r, "seq"), s(&r, "subject"), s(&r, "lang"))}</div>
                                        } }).collect_view()}
                                    })}
                                })
                            }}
                            {move || suggestion.get().map(|p| { let p2 = p.clone(); view! {
                                <div class="small t2" style="margin-top:12px">"Some messages skipped by “Only on” have this value. Their subject starts differently."</div>
                                <button class="link" style="margin-top:6px" on:click=move |_| draft.update(|d| { let cur = s(&d["onlyOn"], "value"); d["onlyOn"]["value"] = json!(if cur.is_empty() { p2.clone() } else { format!("{cur}|{p2}") }); })>{format!("Apply suggestion: also match “{p}”")}</button>
                            } })}
                        }.into_any()
                    }}
                </div>
            </div>
            <Dialog open=yaml_open title="email/extractors.yaml"><pre class="yaml" style="max-height:60vh;overflow:auto">{move || yaml.get()}</pre></Dialog>
            <Confirm open=confirm question=Signal::derive(move || format!("Delete {}?", s(&saved.get(), "name")))
                detail=Signal::derive(move || {
                    let n = s(&saved.get(), "name");
                    let used = data.get()["yours"].as_array().and_then(|a| a.iter().find(|e| s(&e["extractor"], "name") == n).map(|e| e["usedBy"].as_array().map(|u| u.len()).unwrap_or(0))).unwrap_or(0);
                    if used > 0 { format!("{used} inbox step{} use{} it; they will stop finding the value.", if used == 1 { "" } else { "s" }, if used == 1 { "s" } else { "" }) } else { "Tests that wait for this value will time out.".into() }
                })
                action="Delete" on_yes=delete/>
        </div>
    }
}

#[component]
fn CodeEditor(draft: RwSignal<Value>) -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let word = RwSignal::new(String::new());
    view! {
        <h2 class="mono" style="margin:0 0 4px">"code"</h2>
        <div class="row"><Toggle on=Signal::derive(move || draft.get()["on"] == true) set=move |v: bool| draft.update(|d| d["on"] = json!(v))/>
            <span class="small t2">{move || format!("On for e-mail in {}  ·  built-ins can be tuned, not deleted", api::s(&env.get(), "env").to_uppercase())}</span></div>
        <div class="field" style="margin-top:18px"><label>"Digits"</label>
            <div class="row"><span class="t2">"from"</span>
                <input class="input mono" style="width:64px" type="number" prop:value=move || draft.get()["minDigits"].to_string() on:change=move |e| draft.update(|d| d["minDigits"] = json!(event_target_value(&e).parse::<u64>().unwrap_or(4)))/>
                <span class="t2">"to"</span>
                <input class="input mono" style="width:64px" type="number" prop:value=move || draft.get()["maxDigits"].to_string() on:change=move |e| draft.update(|d| d["maxDigits"] = json!(event_target_value(&e).parse::<u64>().unwrap_or(8)))/></div></div>
        <div class="field" style="margin-top:16px"><label>"Words that mark a code"</label>
            <div class="row wrap">
                {move || draft.get()["words"].as_array().cloned().unwrap_or_default().into_iter().map(|w| { let w2 = w.clone(); view! {
                    <span class="chip">{w.as_str().unwrap_or("").to_string()}<button on:click=move |_| draft.update(|d| { if let Some(a) = d["words"].as_array_mut() { a.retain(|x| *x != w2) } })>"×"</button></span> } }).collect_view()}
                <input class="input" style="width:120px;height:26px" placeholder="+ word" prop:value=move || word.get() on:input=move |e| word.set(event_target_value(&e))
                    on:keydown=move |e| if e.key() == "Enter" && !word.get_untracked().trim().is_empty() { let w = word.get_untracked(); draft.update(|d| { if let Some(a) = d["words"].as_array_mut() { a.push(json!(w.trim())) } }); word.set(String::new()); }/>
            </div></div>
        <div class="row" style="margin-top:16px"><Toggle on=Signal::derive(move || draft.get()["preferProminent"] == true) set=move |v: bool| draft.update(|d| d["preferProminent"] = json!(v))/>
            <span>"Prefer the most prominent number: bigger, bold or boxed"</span></div>
    }
}
