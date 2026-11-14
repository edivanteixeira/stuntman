//! SMS (Zenvia API v2): SM1 Messages, SM2 Recipients, SM3 Service settings. One SMS service per
//! environment, so the tabs sit under it (no services column), as WhatsApp.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui::{self, Toggle},
    whatsapp::{pretty, YamlMarked},
};

fn status_class(st: &str) -> &'static str {
    match st {
        "NOT_DELIVERED" | "refused" => "danger",
        "received" => "ok",
        "accepted" | "SENT" => "cap",
        _ => "",
    }
}

const REASONS: [(&str, &str); 5] = [("invalid", "Invalid or inactive number"), ("carrier_rejected", "Carrier rejected (filtered as spam)"), ("phone_off", "Phone off: expired"), ("blocked", "Number blocked by the person"), ("not_covered", "Destination not covered")];

#[component]
fn Head(active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let env = expect_context::<RwSignal<Value>>();
    let live = expect_context::<api::Live>();
    let counts = RwSignal::new((0usize, 0usize));
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move {
            let m = api::get("/sms/messages?since=1d&limit=500").await.ok().and_then(|v| v["messages"].as_array().map(|a| a.len())).unwrap_or(0);
            let r = api::get("/sms/recipients").await.ok().and_then(|v| v["recipients"].as_array().map(|a| a.len())).unwrap_or(0);
            counts.set((m, r));
        });
    });
    let tabs: [(&'static str, &'static str, &'static str); 3] = [("messages", "Messages", "/sms"), ("recipients", "Recipients", "/sms/recipients"), ("service", "Service settings", "/sms/service")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("SMS  ·  answers like Zenvia API v2  ·  {}", s(&env.get(), "env").to_uppercase())}</div>
                    <h1><span class="chan" style="background:#00a3a3"></span>"Zenvia SMS"</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label, path)| { let nav = nav.clone(); view! {
                    <div class="tab" class:on=active == key on:click=move |_| nav(path, Default::default())>{label}
                        <span class="n">{move || match key { "messages" => counts.get().0.to_string(), "recipients" => counts.get().1.to_string(), _ => String::new() }}</span></div> } }).collect_view()}
            </div>
        </div>
    }
}

/// SM1 · Messages.
#[component]
pub fn Messages() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let q = RwSignal::new(String::new());
    let list = RwSignal::new(Vec::<Value>::new());
    let svc = RwSignal::new(Value::Null);
    let reply = RwSignal::new(String::new());
    let nd_open = RwSignal::new(false);
    Effect::new(move |_| {
        live.tick.track();
        let qq = q.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/sms/messages?since=1d&q={}", enc(&qq))).await { list.set(v["messages"].as_array().cloned().unwrap_or_default()); }
            if let Ok(v) = api::get("/sms/service").await { svc.set(v); }
        });
    });
    let sel = Memo::new(move |_| {
        let l = list.get();
        match qm.get().get("id") { Some(id) => l.into_iter().find(|m| s(m, "id") == id).unwrap_or(Value::Null), None => l.into_iter().next().unwrap_or(Value::Null) }
    });
    let act = move |path: String, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        match api::send("POST", &path, body).await { Ok(_) => { if !done.is_empty() { ui::toast(done) } } Err(e) => ui::toast_err(e) }
    });
    let send_reply = move |text: String| {
        let n = s(&sel.get_untracked(), "number");
        if text.trim().is_empty() || n.is_empty() { return; }
        reply.set(String::new());
        act(format!("/sms/numbers/{}/reply", enc(&format!("+{n}"))), json!({"text": text, "to": s(&sel.get_untracked(), "sender")}), "Reply sent to the app as a MESSAGE webhook");
    };
    view! {
        <div class="page">
            <Head active="messages" right=move || view! {
                <input class="input" style="width:200px" placeholder="Number, text, id…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                <span class="row small" style="gap:6px"><span class="dot" class:off=move || !live.connected.get()></span>"Live"</span> }/>
            <div class="body">
                <div class="list" style="width:368px">
                    {move || list.get().into_iter().map(|m| {
                        let id = s(&m, "id");
                        let (i1, i2) = (id.clone(), id.clone());
                        let nav = nav.clone();
                        let st = s(&m, "status");
                        let inbound = s(&m, "dir") == "in";
                        view! {
                            <div class="mrow" class:on=move || s(&sel.get(), "id") == i1 on:click=move |_| nav(&format!("/sms?id={}", enc(&i2)), Default::default())>
                                <div class="mono">{pretty(&s(&m, "number"))}</div>
                                <div class="ell">{if inbound { format!("↩ {}", s(&m, "text")) } else { s(&m, "text") }}</div>
                                <div class="row small"><span class=format!("mono {}", status_class(&st))>{st.to_lowercase().replace('_', " ")}</span><span class="grow"></span>
                                    <span class="cap">{if st == "refused" || st == "NOT_DELIVERED" { if s(&m, "reason").is_empty() { "—".into() } else { REASONS.iter().find(|r| r.0 == s(&m, "reason")).map(|r| r.1.to_string()).unwrap_or(s(&m, "reason")) } }
                                        else if inbound { format!("reply · to {}", s(&m, "sender")) } else { format!("{} segment{} · {}", api::n(&m, "segments"), if api::n(&m, "segments") == 1 { "" } else { "s" }, s(&m, "encoding")) }}</span></div>
                            </div>
                        }
                    }).collect_view()}
                    {move || list.get().is_empty().then(|| { let url = s(&svc.get(), "apiUrl"); let u2 = url.clone(); view! {
                        <div class="callout" style="margin:20px"><div class="t">{format!("No SMS in {}", s(&env.get(), "env").to_uppercase())}</div>
                            <div class="t2">"Point the app's Zenvia URL at Stuntman. Any X-API-TOKEN works."</div>
                            <pre class="yaml envyaml" style="margin:10px 0;white-space:pre-wrap">{format!("SMS_API_URL={url}")}</pre>
                            <button class="link" on:click=move |_| ui::copy(&u2, "URL")>"Copy"</button></div> } })}
                    <div class="small cap" style="padding:14px 24px">"One row per message. Replies from the person show with ↩."</div>
                </div>
                <div class="pane" style="padding:18px 24px;overflow:auto">
                    {move || { let m = sel.get(); (!m.is_null()).then(|| {
                        let tr = s(&m, "traceId");
                        let inbound = s(&m, "dir") == "in";
                        let units = api::n(&m, "units");
                        let max = if s(&m, "encoding") == "UCS-2" { 70 } else { 160 };
                        let ex = m["extracted"].clone();
                        let tpl = m["body"]["template"].clone();
                        view! {
                            <div class="row"><span class="mono" style="font-size:20px;font-weight:600">{pretty(&s(&m, "number"))}</span>
                                <span class="t2" style="margin-left:14px">{if inbound { format!("to “{}”", s(&m, "sender")) } else { format!("from “{}”", s(&m, "sender")) }}</span></div>
                            <div class="row small cap" style="margin:4px 0 14px"><span>{if inbound { "the person → the app (MESSAGE webhook)".to_string() } else { format!("{} → Zenvia API v2  ·  {}", if s(&m, "caller").is_empty() { "app".into() } else { s(&m, "caller") }, api::hm(api::n(&m, "at"))) }}</span>
                                <span class="grow"></span>{(!tr.is_empty()).then(|| view! { <a class="link" href=format!("/telemetry/traces/{tr}")>{format!("trace {} ↗", &tr[..8])}</a> })}</div>
                            <div class="sms-phone">
                                <div class="cap small" style="text-align:center">{s(&m, "sender")}</div>
                                <div class="sms-bubble" class:in=inbound>{s(&m, "text")}<div class="mono small cap" style="margin-top:8px">{api::hm(api::n(&m, "at"))}</div></div>
                                {(!ex.is_null()).then(|| view! { <div class="small ok mono" style="margin-top:10px">{format!("otp = {}   found in {}", s(&ex, "otp"), s(&ex, "from"))}</div> })}
                                {(!tpl.is_null()).then(|| view! { <div class="small cap" style="margin-top:6px">{format!("template {}{}  ·  fields {}", s(&tpl, "templateId"), if s(&tpl, "name").is_empty() { "  ·  not declared (Service settings › Templates)".to_string() } else { format!(" “{}”", s(&tpl, "name")) }, tpl["fields"])}</div> })}
                                <div class="small cap" style="margin-top:14px">"SMS has no read receipts. The person can reply below; replies go to the app as a MESSAGE webhook."</div>
                            </div>
                            {(!inbound).then(|| view! {
                                <div class="caps" style="margin:22px 0 8px">"Encoding and segments"</div>
                                <div class="bar2"><i style=format!("width:{}%", (units * 100 / max).min(100))></i></div>
                                <div class="mono small" style="margin-top:6px">{format!("{units} / {max} characters  ·  {}  ·  {} segment{}", s(&m, "encoding"), api::n(&m, "segments"), if api::n(&m, "segments") == 1 { "" } else { "s" })}</div>
                                <div class="small cap">{if s(&m, "forcedBy").is_empty() { "One character outside GSM-7 (é is in, ó or an emoji is not) switches to UCS-2: 70 per segment, 67 when split.".to_string() } else { format!("UCS-2 because of “{}”. Without it the text fits GSM-7 (160).", s(&m, "forcedBy")) }}</div>
                            })}
                            {(s(&m, "status") == "refused").then(|| view! { <div class="callout danger-fill small" style="margin-top:16px">{format!("Refused: {}. Answered with Zenvia's error body; no id, no callback.", s(&m, "reason"))}</div> })}
                            <div class="small cap" style="margin-top:24px">{format!("Reply as {}", pretty(&s(&m, "number")))}</div>
                            <input class="input" style="width:100%;max-width:460px;margin:6px 0;display:block" placeholder="Type what the person replies…" prop:value=move || reply.get() on:input=move |e| reply.set(event_target_value(&e))
                                on:keydown=move |e| if e.key() == "Enter" { send_reply(reply.get_untracked()) }/>
                            <div class="row" style="gap:8px;max-width:460px">
                                {["STOP", "HELP", "Yes"].into_iter().map(|w| view! { <button class="btn small" on:click=move |_| send_reply(w.to_string())>{w}</button> }).collect_view()}
                                <span class="grow"></span><button class="btn primary small" on:click=move |_| send_reply(reply.get_untracked())>"Send"</button></div>
                            <div class="small cap" style="margin-top:8px">"STOP adds the number to the opt-out list; the next send is refused until the person texts START."</div>
                        }
                    }) }}
                </div>
                <div class="side envside" style="width:360px">
                    {move || { let m = sel.get(); (!m.is_null()).then(|| {
                        let id = s(&m, "id");
                        let inbound = s(&m, "dir") == "in";
                        let (id1, id2) = (id.clone(), id.clone());
                        view! {
                            <div class="caps">"This message"</div><div class="mono small" style="margin:6px 0 16px;word-break:break-all">{id.clone()}</div>
                            <div class="caps">{if inbound { "Delivered to the app" } else { "Status, as the app saw it" }}</div>
                            {m["history"].as_array().cloned().unwrap_or_default().into_iter().map(|h| { let w = h["webhook"].clone(); view! {
                                <div class="row" style="align-items:flex-start;gap:12px;margin-top:10px"><span class="mono small cap">{api::hm(api::n(&h, "at"))}</span>
                                    <div><div class=status_class(&s(&h, "status"))>{s(&h, "status")}{(!s(&h, "reason").is_empty()).then(|| format!("  ·  {}", s(&h, "reason")))}</div>
                                        <div class="small cap">{if s(&h, "status") == "accepted" { "200 · id returned".to_string() } else if w.is_null() { if s(&h, "by").is_empty() { String::new() } else { format!("by {}", s(&h, "by")) } }
                                            else if !s(&w, "error").is_empty() { format!("callback → {}", s(&w, "error")) } else { format!("callback → {} {}", s(&w, "url").split('/').nth(2).unwrap_or(""), w["status"]) }}</div></div></div> } }).collect_view()}
                            {(!inbound && s(&m, "status") != "refused").then(|| view! {
                                <div class="caps" style="margin-top:22px">"Provoke on this message"</div>
                                <div class="row" style="gap:8px;margin-top:8px">
                                    <span class="anchor"><button class="btn small" on:click=move |_| nd_open.set(true)>"Not delivered ▾"</button>
                                        <ui::Pop open=nd_open>
                                            <div class="mh caps">"Reason sent in the callback"</div>
                                            {REASONS.into_iter().map(|(k, l)| { let id = id1.clone(); view! { <div class="mi" on:click=move |_| act(format!("/sms/messages/{}/status", enc(&id)), json!({"status": "NOT_DELIVERED", "reason": k}), "NOT_DELIVERED sent to the app")><span>{l}</span></div> } }).collect_view()}
                                        </ui::Pop></span>
                                    <button class="btn small" on:click=move |_| act(format!("/sms/messages/{}/resend", enc(&id2)), json!({}), "Duplicate callback sent")>{format!("Send {} again", s(&m, "status"))}</button>
                                </div>
                                <div class="small cap" style="margin-top:6px">"Not delivered sends NOT_DELIVERED with the reason you pick; again sends a duplicate callback."</div>
                                <div class="caps" style="margin-top:22px">"Next messages to this number"</div>
                                <a class="link small" href="/sms/recipients">"From the recipient rule; change it in Recipients ›"</a>
                            })}
                            <div class="caps" style="margin-top:22px">"Details"</div>
                            <table class="kv2" style="margin-top:6px">
                                <tr><td>"Encoding"</td><td class="mono">{format!("{} · {} segment(s)", s(&m, "encoding"), api::n(&m, "segments"))}</td></tr>
                                <tr><td>"externalId"</td><td class="mono small" style="word-break:break-all">{s(&m, "externalId")}</td></tr>
                                <tr><td>"Callback"</td><td class="mono small" style="word-break:break-all">{let c = s(&svc.get()["service"]["callbacks"], if inbound { "inbound" } else { "status" }); if c.is_empty() { let d = s(&svc.get(), "callbackDefault"); if d.is_empty() { "none set".into() } else { d } } else { c }}</td></tr>
                            </table>
                        }
                    }) }}
                </div>
            </div>
        </div>
    }
}

fn summary(r: &Value) -> String {
    let mut p = vec![];
    for st in r["steps"].as_array().cloned().unwrap_or_default() {
        if let Some(v) = st["delivered_after"].as_str() { p.push(format!("delivered in {v}")); }
        if let Some(v) = st["not_delivered"].as_str() { p.push(format!("not delivered · {}", REASONS.iter().find(|x| x.0 == v).map(|x| x.1).unwrap_or(v))); }
        if let Some(v) = st["never_delivered"].as_str() { p.push(format!("phone off · expires after {v}")); }
        if st["on_question"].is_object() { p.push(format!("replies “{}” to questions", s(&st["on_question"], "reply"))); }
        if st["on_nth"].is_object() { p.push(format!("text #{}: “{}”", api::n(&st["on_nth"], "n"), s(&st["on_nth"], "reply"))); }
        if st["on_text"].is_object() { p.push(format!("always replies “{}”", s(&st["on_text"], "reply"))); }
    }
    if p.is_empty() { "delivered in 3 s".into() } else { p.join("  ·  ") }
}

fn step_get(d: &Value, key: &str, field: &str) -> String {
    d["steps"].as_array().and_then(|a| a.iter().find_map(|st| st.get(key).map(|v| if field.is_empty() { v.as_str().map(String::from).unwrap_or(v.to_string()) } else { v[field].as_str().map(String::from).unwrap_or(if v[field].is_null() { String::new() } else { v[field].to_string() }) }))).unwrap_or_default()
}

fn step_set(d: &mut Value, key: &str, field: &str, val: String) {
    let steps = d["steps"].as_array_mut();
    let Some(steps) = steps else { d["steps"] = json!([]); return step_set(d, key, field, val) };
    let pos = steps.iter().position(|st| st.get(key).is_some());
    let empty = val.trim().is_empty();
    match (pos, field.is_empty()) {
        (Some(i), true) => if empty { steps.remove(i); } else { steps[i][key] = json!(val); },
        (None, true) => if !empty { steps.push(json!({key: val})); },
        (Some(i), false) => {
            if field == "reply" && empty { steps.remove(i); return; }
            steps[i][key][field] = if field == "n" { json!(val.parse::<u32>().unwrap_or(1)) } else { json!(val) };
        }
        (None, false) => if !empty { steps.push(json!({key: {field: if field == "n" { json!(val.parse::<u32>().unwrap_or(1)) } else { json!(val) }}})); },
    }
}

/// SM2 · Recipients.
#[component]
pub fn Recipients() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let data = RwSignal::new(Value::Null);
    let sel = RwSignal::new(String::from("*"));
    let draft = RwSignal::new(Value::Null);
    let last = RwSignal::new(Vec::<Value>::new());
    let load = move || leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/sms/recipients").await {
            let cur = v["recipients"].as_array().and_then(|a| a.iter().find(|r| s(r, "number") == sel.get_untracked()).cloned()).unwrap_or_else(|| v["recipients"][0].clone());
            sel.set(s(&cur, "number"));
            draft.set(cur);
            data.set(v);
        }
    });
    load();
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("sms") && draft.get_untracked()["_dirty"] != true { load(); } t });
    Effect::new(move |_| {
        let n = sel.get();
        leptos::task::spawn_local(async move {
            let v = if n.contains('*') { Value::Null } else { api::get(&format!("/sms/messages?number={}&since=1d&limit=5", enc(&n))).await.unwrap_or(Value::Null) };
            last.set(v["messages"].as_array().cloned().unwrap_or_default());
        });
    });
    let dirty = Memo::new(move |_| draft.get()["_dirty"] == true);
    let save = move |_| {
        let mut d = draft.get_untracked();
        if let Some(o) = d.as_object_mut() { o.remove("_dirty"); }
        leptos::task::spawn_local(async move {
            match api::send("PUT", "/sms/recipients", json!({"number": d["number"], "carrier": d["carrier"], "steps": d["steps"]})).await {
                Ok(_) => { ui::toast("Saved in sms/recipients.yaml"); sel.set(s(&d, "number")); load(); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let edit = move |f: Box<dyn FnOnce(&mut Value)>| draft.update(|d| { f(d); d["_dirty"] = json!(true); });
    let field = move |label: &'static str, key: &'static str, sub: &'static str, ph: &'static str, w: &'static str| view! {
        <input class="input mono" style=format!("width:{w}") placeholder=ph prop:value=move || step_get(&draft.get(), key, sub) on:change=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| step_set(d, key, sub, v))) } title=label/>
    };
    view! {
        <div class="page">
            <Head active="recipients" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"Unsaved change"</span><button class="btn" on:click=move |_| load()>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button> }/>
            <div class="body">
                <div class="list" style="width:340px">
                    {move || data.get()["recipients"].as_array().cloned().unwrap_or_default().into_iter().map(|r| {
                        let n = s(&r, "number");
                        let (n1, n2) = (n.clone(), n.clone());
                        let bad = r["steps"].as_array().map(|a| a.iter().any(|x| x.get("not_delivered").is_some())).unwrap_or(false);
                        view! { <div class="mrow" class:on=move || sel.get() == n1 on:click=move |_| { sel.set(n2.clone()); load(); }>
                            <div class=if n == "*" { "" } else { "mono" }>{if n == "*" { "Everyone else".to_string() } else { n.clone() }}</div>
                            <div class="small" class:danger=bad class:t2=!bad>{summary(&r)}</div></div> }
                    }).collect_view()}
                    <div style="padding:12px 24px"><button class="link" on:click=move |_| { sel.set(String::new()); draft.set(json!({"number": "+55119", "carrier": "", "steps": [{"delivered_after": "3s"}], "_dirty": true})); }>"+ Add recipient"</button></div>
                    <div class="small cap" style="padding:0 24px">"The simulated person behind a number: whether the text arrives, how long the carrier takes and what they reply. First match wins; patterns end with *."</div>
                </div>
                <div class="pane pad form"><div style="max-width:620px">
                    {move || { let d = draft.get(); (!d.is_null()).then(|| { let star = s(&d, "number") == "*"; view! {
                        <div class="row" style="gap:12px;margin-bottom:10px"><span style="width:90px">"Number"</span>
                            <input class="input mono" style="width:220px" disabled=star prop:value=s(&d, "number") on:change=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| d["number"] = json!(v))) }/></div>
                        <div class="row" style="gap:12px;margin-bottom:4px"><span style="width:90px">"Carrier"</span>
                            <input class="input" style="width:220px" placeholder="Vivo, Claro, TIM…" prop:value=s(&d, "carrier") on:change=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| d["carrier"] = json!(v))) }/></div>
                        <div class="small cap" style="margin-left:102px">"shown in Notifications"</div>
                        <div class="caps" style="margin:24px 0 10px">"What happens to each text"</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"Delivered after"</span>{field("delivered after", "delivered_after", "", "3s", "90px")}</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"Not delivered, with reason"</span>
                            <select class="input" style="width:260px" on:change=move |e| { let v = event_target_value(&e); edit(Box::new(move |d| step_set(d, "not_delivered", "", v))) }>
                                <option value="" selected=step_get(&d, "not_delivered", "").is_empty()>"no (delivered)"</option>
                                {REASONS.into_iter().map(|(k, l)| view! { <option value=k selected=step_get(&d, "not_delivered", "") == k>{l}</option> }).collect_view()}</select></div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"Phone off: expires after"</span>{field("never delivered", "never_delivered", "", "empty = never; 48h", "140px")}</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"When the text asks something"</span>{field("reply", "on_question", "reply", "reply…", "200px")}<span class="small cap">"after"</span>{field("after", "on_question", "after", "30s", "70px")}</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"On text number"</span>{field("n", "on_nth", "n", "3", "50px")}<span class="small cap">"of the day, reply"</span>{field("reply", "on_nth", "reply", "STOP", "120px")}</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:190px">"Always reply"</span>{field("reply", "on_text", "reply", "reply…", "200px")}<span class="small cap">"after"</span>{field("after", "on_text", "after", "10s", "70px")}</div>
                        <div class="small cap" style="margin-top:16px">"“Asks something” means the text ends with a question mark or contains “reply”. STOP adds the number to the opt-out list."</div>
                        {(!star && !s(&d, "number").is_empty()).then(|| { let n = s(&d, "number"); view! {
                            <button class="link danger" style="margin-top:20px" on:click=move |_| { let n = n.clone(); leptos::task::spawn_local(async move {
                                match api::send("DELETE", "/sms/recipients", json!({"number": n})).await { Ok(_) => { ui::toast("Removed"); sel.set("*".into()); load(); } Err(e) => ui::toast_err(e) } }) }>"Remove recipient"</button> } })}
                    } }) }}
                </div></div>
                <div class="side envside" style="width:400px">
                    <div class="caps">"Last texts to this number"</div>
                    {move || { let l = last.get(); if l.is_empty() { view! { <div class="small cap" style="margin:8px 0 18px">"None in the last day."</div> }.into_any() } else { l.into_iter().map(|m| view! {
                        <div class="row" style="align-items:flex-start;gap:12px;margin-top:8px"><span class="mono small cap">{api::hm(api::n(&m, "at"))}</span>
                            <div><div class="ell" style="max-width:280px">{s(&m, "text")}</div><div class="small cap">{s(&m, "status").to_lowercase().replace('_', " ")}</div></div></div> }).collect_view().into_any() } }}
                    <div class="caps" style="margin-top:22px">"sms/recipients.yaml"</div>
                    <YamlMarked text=Signal::derive(move || s(&data.get(), "yaml")) before=Signal::derive(String::new)/>
                </div>
            </div>
        </div>
    }
}

/// SM3 · Service settings.
#[component]
pub fn Service() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let data = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let before = RwSignal::new(String::new());
    let load = move || leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/sms/service").await {
            let old = s(&data.get_untracked(), "yaml");
            if !old.is_empty() && old != s(&v, "yaml") { before.set(old); }
            draft.set(v["service"].clone());
            data.set(v);
        }
    });
    load();
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && ["sms.service", "sms.optouts"].contains(&s(&live.last.get_untracked(), "topic").as_str()) && draft.get_untracked() == data.get_untracked()["service"] { load(); } t });
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != data.get()["service"]);
    let save = move |_| {
        let d = draft.get_untracked();
        leptos::task::spawn_local(async move {
            let body = json!({"any_token": d["access"]["any_token"], "strict_senders": d["strict_senders"], "senders": d["senders"], "callbacks": d["callbacks"], "opt_out": d["opt_out"], "limits": d["limits"], "templates": d["templates"], "strict_templates": d["strict_templates"]});
            match api::send("PUT", "/sms/service", body).await { Ok(_) => { ui::toast("Saved in sms/service.yaml"); load(); } Err(e) => ui::toast_err(e) }
        });
    };
    let set = move |path: &'static str, v: Value| draft.update(|d| { if let Some(p) = d.pointer_mut(path) { *p = v; } });
    view! {
        <div class="page">
            <Head active="service" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"Unsaved change"</span><button class="btn" on:click=move |_| draft.set(data.get_untracked()["service"].clone())>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button> }/>
            <div class="body">
                <div class="pane pad form"><div style="max-width:760px">
                    {move || { let d = draft.get(); let v = data.get(); (!d.is_null()).then(|| { let url = s(&v, "apiUrl"); let u2 = url.clone(); view! {
                        <div class="caps">"API"</div>
                        <table class="kv2" style="margin:8px 0 14px">
                            <tr><td>"Answers like"</td><td>"Zenvia API v2"</td><td class="small cap">"Twilio is not in this build"</td></tr>
                            <tr><td>"API URL"</td><td class="mono small">{url}</td><td><button class="link" on:click=move |_| ui::copy(&u2, "URL")>"Copy"</button></td></tr>
                        </table>
                        <div class="row" style="gap:10px;margin:8px 0"><Toggle on=Signal::derive(move || draft.get()["access"]["any_token"] == true) set=move |b: bool| set("/access/any_token", json!(b))/>
                            <span>"Accept any X-API-TOKEN"</span><span class="small cap">"off: only Secrets › zenvia-token"</span></div>
                        <div class="caps" style="margin-top:22px">"Senders"</div>
                        {d["senders"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, x)| view! {
                            <div class="row" style="gap:16px;margin:6px 0"><span class="mono" style="width:160px">{s(&x, "id")}</span><span class="t2" style="width:120px">{s(&x, "type").replace('_', " ")}</span>
                                <button class="link plain" on:click=move |_| draft.update(|d| { if let Some(a) = d["senders"].as_array_mut() { a.remove(i); } })>"Remove"</button></div> }).collect_view()}
                        <div class="row" style="gap:8px;margin-top:6px"><button class="link" on:click=move |_| draft.update(|d| { if let Some(a) = d["senders"].as_array_mut() { a.push(json!({"id": format!("Sender{}", a.len() + 1), "type": "alphanumeric"})); } })>"+ Add sender"</button></div>
                        <div class="row" style="gap:10px;margin:8px 0"><Toggle on=Signal::derive(move || draft.get()["strict_senders"] == true) set=move |b: bool| set("/strict_senders", json!(b))/>
                            <span>"Refuse an unknown “from” (400)"</span><span class="small cap">"as Zenvia does; off accepts any"</span></div>
                        <div class="caps" style="margin-top:22px">"Callbacks to the app"</div>
                        <div class="field"><label>"MESSAGE_STATUS"</label><input class="input mono" placeholder="empty: the environment's callback" prop:value=s(&d["callbacks"], "status") on:input=move |e| set("/callbacks/status", json!(event_target_value(&e)))/></div>
                        <div class="field"><label>"MESSAGE (replies)"</label><input class="input mono" placeholder="empty: the environment's callback" prop:value=s(&d["callbacks"], "inbound") on:input=move |e| set("/callbacks/inbound", json!(event_target_value(&e)))/></div>
                        <div class="caps" style="margin-top:22px">"Templates"</div>
                        <div class="small cap" style="margin:4px 0 8px">"The app sends contents of type template (templateId + fields). Declare the text to see what the person reads; {{field}} takes the app's value."</div>
                        {d["templates"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, t)| view! {
                            <div class="row" style="gap:8px;margin:6px 0;align-items:flex-start">
                                <input class="input mono small" style="width:150px" placeholder="templateId" prop:value=s(&t, "id") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["templates"][i]["id"] = json!(v)) }/>
                                <input class="input" style="width:140px" placeholder="name" prop:value=s(&t, "name") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["templates"][i]["name"] = json!(v)) }/>
                                <input class="input grow" placeholder="Acme: your code is {{token}}" prop:value=s(&t, "text") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["templates"][i]["text"] = json!(v)) }/>
                                <button class="link plain" on:click=move |_| draft.update(|d| { if let Some(a) = d["templates"].as_array_mut() { a.remove(i); } })>"×"</button></div> }).collect_view()}
                        <button class="link" on:click=move |_| draft.update(|d| { if let Some(a) = d["templates"].as_array_mut() { a.push(json!({"id": "", "name": "", "text": ""})); } })>"+ Add template"</button>
                        <div class="row" style="gap:10px;margin:8px 0"><Toggle on=Signal::derive(move || draft.get()["strict_templates"] == true) set=move |b: bool| set("/strict_templates", json!(b))/>
                            <span>"Refuse an undeclared templateId (400)"</span><span class="small cap">"off: sent, fields shown"</span></div>
                        <div class="caps" style="margin-top:22px">"Opt-out"</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:110px">"Keywords"</span><span class="mono">{d["opt_out"]["keywords"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("")).collect::<Vec<_>>().join("  ")).unwrap_or_default()}</span>
                            <span class="cap">"·  back in:"</span><span class="mono">{s(&d["opt_out"], "back_in")}</span></div>
                        <div class="row" style="gap:10px;align-items:flex-start"><span style="width:110px">"On the list"</span><div>
                            {v["optOuts"].as_array().cloned().unwrap_or_default().into_iter().map(|o| { let n = s(&o, "number"); view! {
                                <div class="row" style="gap:14px;margin-bottom:4px"><span class="mono">{pretty(&n)}</span><span class="small cap">{format!("since {}  ·  {}", api::hm(api::n(&o, "at")), s(&o, "by"))}</span>
                                    <button class="link" on:click=move |_| { let n = n.clone(); leptos::task::spawn_local(async move { let _ = api::send("DELETE", &format!("/sms/opt-outs/{}", enc(&n)), json!({})).await; load(); }) }>"Remove"</button></div> } }).collect_view()}
                            {v["optOuts"].as_array().map(|a| a.is_empty()).unwrap_or(true).then(|| view! { <span class="small cap">"nobody"</span> })}</div></div>
                        <div class="caps" style="margin-top:22px">"Limits"</div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:110px">"Throughput"</span>
                            <input class="input mono" style="width:90px" type="number" prop:value=d["limits"]["throughput"].to_string() on:change=move |e| set("/limits/throughput", json!(event_target_value(&e).parse::<u32>().unwrap_or(30)))/><span class="small cap">"messages/s  ·  over it: 429"</span></div>
                        <div class="row" style="gap:10px;margin:8px 0"><span style="width:110px">"Body size"</span>
                            <input class="input mono" style="width:90px" type="number" prop:value=d["limits"]["max_segments"].to_string() on:change=move |e| set("/limits/max_segments", json!(event_target_value(&e).parse::<u32>().unwrap_or(10)))/><span class="small cap">"segments  ·  over it: 400 “message too long”"</span></div>
                        <div class="small cap" style="margin-top:16px">"Fields come from sms/service.yaml. The token is a secret:// reference; Stuntman never shows its value."</div>
                    } }) }}
                </div></div>
                <div class="side envside" style="width:420px">
                    <div class="caps">"sms/service.yaml"<span class="ok" style="margin-left:8px">"live"</span></div>
                    <YamlMarked text=Signal::derive(move || s(&data.get(), "yaml")) before=before/>
                    <a class="link small" style="display:inline-block;margin-top:8px" target="_blank" href=api::url("/api/sms/files/service.yaml")>"Open the file ›"</a>
                </div>
            </div>
        </div>
    }
}
