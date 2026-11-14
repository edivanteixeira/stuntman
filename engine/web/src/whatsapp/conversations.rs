//! W1 · Conversations: one row per number; the chat with the 24 h window; what the app saw of the
//! selected message (status webhooks and the app's answers), the recipient rule and provocations.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{error_name, pretty, status_class, Head};
use crate::{
    api::{self, enc, s},
    ui::{self, Dialog, Pop},
};

fn left(ms: i64) -> String {
    let m = ms / 60_000;
    if m >= 60 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }
}

#[component]
pub fn Conversations() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let q = RwSignal::new(String::new());
    let convs = RwSignal::new(Vec::<Value>::new());
    let msgs = RwSignal::new(Vec::<Value>::new());
    let tpls = RwSignal::new(Vec::<Value>::new());
    let explain = RwSignal::new(Value::Null);
    let number = super::number();
    let hooks = RwSignal::new(Vec::<Value>::new());
    let draft = RwSignal::new(String::new());
    let fail_open = RwSignal::new(false);
    let hooks_open = RwSignal::new(false);
    let sel_num = Memo::new(move |_| qm.get().get("number").or_else(|| convs.get().first().map(|c| s(c, "number"))).unwrap_or_default());
    let load = move || {
        let (qq, n) = (q.get_untracked(), sel_num.get_untracked());
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/whatsapp/conversations{}", if qq.is_empty() { String::new() } else { format!("?q={}", enc(&qq)) })).await { convs.set(v["conversations"].as_array().cloned().unwrap_or_default()); }
            if let Ok(v) = api::get("/whatsapp/templates").await { tpls.set(v["templates"].as_array().cloned().unwrap_or_default().into_iter().map(|t| t["template"].clone()).collect()); }
            if let Ok(v) = api::get("/whatsapp/webhooks?limit=40").await { hooks.set(v["webhooks"].as_array().cloned().unwrap_or_default()); }
            if !n.is_empty() {
                if let Ok(v) = api::get(&format!("/whatsapp/messages?number={}&since=7d&limit=300", enc(&n))).await { let mut m = v["messages"].as_array().cloned().unwrap_or_default(); m.reverse(); msgs.set(m); }
                if let Ok(v) = api::get(&format!("/whatsapp/recipients/explain?number={}", enc(&format!("+{n}")))).await { explain.set(v); }
            } else { msgs.set(vec![]); }
        });
    };
    Effect::new(move |_| { sel_num.track(); q.track(); load(); });
    let paused = RwSignal::new(false);
    let missed = RwSignal::new(0i64);
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("whatsapp.") { if paused.get_untracked() { missed.update(|m| *m += 1) } else { load() } } t });
    let sel = Memo::new(move |_| {
        let w = qm.get().get("wamid");
        let m = msgs.get();
        w.and_then(|w| m.iter().find(|x| s(x, "id") == w).cloned()).or_else(|| m.iter().rev().find(|x| s(x, "dir") == "out").cloned()).unwrap_or(Value::Null)
    });
    let tpl_of = move |name: &str| tpls.get().into_iter().find(|t| s(t, "name") == name).unwrap_or(Value::Null);
    let send = move |body: Value| {
        let n = sel_num.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/whatsapp/numbers/{}/reply", enc(&format!("+{n}"))), body).await {
                Ok(_) => { draft.set(String::new()); ui::toast("Sent to the app as the messages webhook") }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let set_status = move |wamid: String, st: &'static str, code: Option<i64>| leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/whatsapp/messages/{}/status", enc(&wamid)), json!({"status": st, "error_code": code})).await {
            Ok(_) => ui::toast(format!("Marked {st}; the status webhook went to the app")), Err(e) => ui::toast_err(e) }
    });
    let resend = move |wamid: String| leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/whatsapp/messages/{}/resend", enc(&wamid)), json!({"status": "read"})).await { Ok(_) => ui::toast("“read” sent again"), Err(e) => ui::toast_err(e) }
    });
    // Failed status webhooks not taken later (newest first: a 2xx for the same message clears older failures).
    let unresolved = Memo::new(move |_| {
        let mut ok = std::collections::HashSet::new();
        let mut out = vec![];
        for h in hooks.get() {
            if h["kind"] != "status" { continue; }
            let w = s(&h, "wamid");
            if h["status"].as_i64().map(|c| (200..300).contains(&c)).unwrap_or(false) { ok.insert(w); } else if !ok.contains(&w) { out.push(h); }
        }
        out
    });
    let failing = Memo::new(move |_| unresolved.get().len());
    let nav2 = nav.clone();
    view! {
        <div class="page">
            <Head active="conversations" right=move || view! {
                <input class="input" style="width:190px" placeholder="Number, text, wamid…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                <span class="row small" style="gap:6px"><span class="dot" class:off=move || paused.get() || !live.connected.get()></span>
                    {move || if paused.get() { view! { <span class="t2">"Paused"</span>{(missed.get() > 0).then(|| view! { <span class="attn">{format!("  {} new", missed.get())}</span> })}
                        <span class="cap">"·"</span><button class="link" on:click=move |_| { paused.set(false); missed.set(0); load(); }>"Resume"</button> }.into_any() }
                        else { view! { <span style="font-weight:600">"Live"</span><span class="cap">"·"</span><button class="link plain" on:click=move |_| paused.set(true)>"Pause"</button> }.into_any() }}</span>
            }/>
            {move || (failing.get() > 0).then(|| {
                let code = unresolved.get().first().cloned().unwrap_or(Value::Null);
                let msgs_n = unresolved.get().iter().map(|h| s(h, "wamid")).collect::<std::collections::HashSet<_>>().len();
                view! { <div class="strip">{format!("The app answered {} to the status webhooks of {} message(s). Meta retries for days; Stuntman retries 3 times in a minute, then waits here.",
                    code["status"].as_i64().map(|c| c.to_string()).unwrap_or(s(&code, "error")), msgs_n)}
                    <button class="btn small" on:click=move |_| leptos::task::spawn_local(async move {
                        match api::send("POST", "/whatsapp/webhooks/retry", json!({})).await { Ok(v) => ui::toast(format!("{} webhook(s) sent again", api::n(&v, "retried"))), Err(e) => ui::toast_err(e) } })>"Retry now"</button>
                    <button class="link" on:click=move |_| hooks_open.set(true)>{format!("Show the {} deliveries", failing.get())}</button></div> }
            })}
            <div class="body">
                <div class="list" style="width:368px">
                    {move || convs.get().into_iter().map(|c| {
                        let n = s(&c, "number");
                        let (n1, n2) = (n.clone(), n.clone());
                        let last = c["last"].clone();
                        let w = c["window"].clone();
                        let st = if s(&last, "dir") == "in" { "replied".to_string() } else { s(&last, "status") };
                        let err = last["error"].as_i64();
                        let nav = nav.clone();
                        view! {
                            <div class="mrow wa" class:on=move || sel_num.get() == n1 on:click=move |_| nav(&format!("/whatsapp?number={}", enc(&n2)), Default::default())>
                                <div class="row"><span class="mono">{pretty(&n)}</span><span class="t2" style="margin-left:14px">{s(&c, "profileName")}</span></div>
                                <div class="ell">{if s(&last, "dir") == "in" { format!("↩ {}", s(&last, "text")) } else if !s(&last, "template").is_empty() && s(&last, "text").is_empty() { s(&last, "template") } else { s(&last, "text") }}</div>
                                <div class="row small"><span class=format!("mono {}", status_class(&st))>{format!("{st}{}", err.map(|e| format!(" {e}")).unwrap_or_default())}</span><span class="grow"></span>
                                    <span class="cap">{if w["open"] == true { format!("window open · {} left", left(api::n(&w, "leftMs"))) } else if let Some(e) = err { error_name(e).to_string() } else if s(&last, "status") == "sent" { "waiting for delivery".into() } else { "no reply yet".into() }}</span></div>
                            </div>
                        }
                    }).collect_view()}
                    {move || convs.get().is_empty().then(|| {
                        let n = number.get();
                        let envs = format!("WHATSAPP_GRAPH_URL={}\nWHATSAPP_PHONE_NUMBER_ID={}", s(&n, "graphBase"), s(&n["number"], "phone_number_id"));
                        let e2 = envs.clone();
                        view! { <div class="callout" style="margin:20px">
                            <div class="t">{format!("No WhatsApp messages in {}", s(&env.get(), "env").to_uppercase())}</div>
                            <div class="t2">"Point the app's Graph API base URL at Stuntman:"</div>
                            <pre class="yaml envyaml" style="margin:10px 0">{envs}</pre>
                            <button class="link" on:click=move |_| ui::copy(&e2, "settings")>"Copy"</button></div> }
                    })}
                    <div class="small cap" style="padding:14px 24px">"One row per number. The window is the 24 h after the person last wrote; outside it only templates are delivered."</div>
                </div>
                <div class="pane" style="display:flex;flex-direction:column;padding:16px 24px;gap:12px;overflow:auto">
                    {move || {
                        let n = sel_num.get();
                        if n.is_empty() { return view! { <div></div> }.into_any(); }
                        let ex = explain.get();
                        let w = ex["window"].clone();
                        let num = number.get();
                        let last = msgs.get().into_iter().rev().find(|m| s(m, "dir") == "out").unwrap_or(Value::Null);
                        let tr = s(&last, "traceId");
                        view! {
                            <div class="row"><span class="mono" style="font-size:18px;font-weight:600">{pretty(&n)}</span>
                                <span class="t2" style="margin-left:14px">{s(&ex["match"], "profile_name")}</span><span class="small cap" style="margin-left:8px">"profile name sent in webhooks"</span></div>
                            <div class="row small cap"><span>{format!("{} → number {}  ·  Cloud API", if s(&last, "caller").is_empty() { "app".into() } else { s(&last, "caller") }, s(&num["number"], "phone_number_id"))}</span><span class="grow"></span>
                                {(!tr.is_empty()).then(|| view! { <a class="link" href=format!("/telemetry/traces/{tr}")>{format!("trace {} ↗", &tr[..8])}</a> })}</div>
                            {if w["open"] == true {
                                view! { <div class="callout ok-fill small">{format!("Customer service window open  ·  {} left  ·  since the reply at {}", left(api::n(&w, "leftMs")), api::hm(api::n(&w, "since")))}</div> }.into_any()
                            } else if !w["since"].is_null() {
                                view! { <div class="callout attn-fill small">{format!("Window closed since {}  ·  only templates can be sent", api::hm(api::n(&w, "closedAt")))}</div> }.into_any()
                            } else { view! { <div class="callout small t2">"No reply from this person yet: only templates are delivered."</div> }.into_any() }}
                        }.into_any()
                    }}
                    <div class="wa-chat">
                        {let nav2 = nav2.clone(); move || { let nav2 = nav2.clone(); let mut last_day = String::new(); msgs.get().into_iter().map(move |m| {
                            // A day label before the first message of each day, as on the phone.
                            let day = {
                                let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(api::n(&m, "at") as f64));
                                let today = js_sys::Date::new_0();
                                if d.to_date_string() == today.to_date_string() { "Today".to_string() } else { String::from(d.to_locale_date_string("en-US", &wasm_bindgen::JsValue::UNDEFINED)) }
                            };
                            let new_day = (day != last_day).then(|| { last_day = day.clone(); view! { <div class="caps" style="align-self:center;margin:4px 0">{day.clone()}</div> } });
                            let out = s(&m, "dir") == "out";
                            let id = s(&m, "id");
                            let id2 = id.clone();
                            let t = tpl_of(&s(&m, "template"));
                            let st = s(&m, "status");
                            let nav = nav2.clone();
                            let n = sel_num.get_untracked();
                            let ticks = match st.as_str() { "read" => "✓✓ read", "delivered" => "✓✓ delivered", "sent" => "✓ sent", "failed" => "failed", "accepted" => "accepted", _ => "" };
                            view! {
                                {new_day}
                                <div class="bubble" class:out=out class:on=move || s(&sel.get(), "id") == id on:click=move |_| nav(&format!("/whatsapp?number={}&wamid={}", enc(&n), enc(&id2)), Default::default())>
                                    {(!t.is_null()).then(|| view! { <div class="tbar"></div>{(!s(&t, "header").is_empty()).then(|| view! { <div class="strong">{s(&t, "header")}</div> })} })}
                                    <div class="body-t">{if s(&m, "kind") == "location" { format!("📍 {}", s(&m, "text")) } else if s(&m, "kind") == "image" { format!("🖼 image  ·  {}", s(&m, "text")) } else if s(&m, "kind") == "button" { format!("↩ {}", s(&m, "text")) } else { s(&m, "text") }}</div>
                                    {(!t.is_null() && !s(&t, "footer").is_empty()).then(|| view! { <div class="small cap">{s(&t, "footer")}</div> })}
                                    <div class="meta mono small"><span>{api::hm(api::n(&m, "at"))}</span>
                                        {out.then(|| view! { <span class=status_class(&st)>{format!("  {ticks}{}", m["error"].as_i64().map(|e| format!(" {e}")).unwrap_or_default())}</span> })}
                                        {(!out && s(&m, "by") != "recipient rule" && !s(&m, "by").is_empty()).then(|| view! { <span class="attn">{format!("  ·  sent by {} as the person", s(&m, "by"))}</span> })}</div>
                                    {t["buttons"].as_array().cloned().unwrap_or_default().into_iter().map(|b| view! { <div class="wa-btn">{if s(&b, "url").is_empty() { "↩ " } else { "↗ " }}{s(&b, "text")}</div> }).collect_view()}
                                </div>
                                {(!t.is_null()).then(|| view! { <div class="small cap" class:right=out>{format!("template {}  ·  {}  ·  {}", s(&m, "template"), s(&m, "lang"), s(&m, "category"))}</div> })}
                            }
                        }).collect_view() }}
                        <div class="small cap" style="margin-top:8px">"The person's side is simulated: reply below, tap a button, or let a recipient rule answer."</div>
                    </div>
                    {move || (!sel_num.get().is_empty()).then(|| {
                        let n = sel_num.get();
                        let last_tpl = msgs.get().into_iter().rev().find(|m| !s(m, "template").is_empty()).map(|m| tpl_of(&s(&m, "template"))).unwrap_or(Value::Null);
                        let qr: Vec<String> = last_tpl["buttons"].as_array().cloned().unwrap_or_default().iter().filter(|b| !s(b, "quick_reply").is_empty()).map(|b| s(b, "text")).collect();
                        let name = s(&explain.get()["match"], "profile_name");
                        view! {
                            <div class="small cap">{format!("Reply as {}", pretty(&n))}</div>
                            <input class="input" placeholder=format!("Type what {} writes back…", if name.is_empty() { "the person".into() } else { name.split(' ').next().unwrap_or("").to_string() })
                                prop:value=move || draft.get() on:input=move |e| draft.set(event_target_value(&e))
                                on:keydown=move |e| if e.key() == "Enter" && !draft.get_untracked().is_empty() { send(json!({"text": draft.get_untracked()})) }/>
                            <div class="row" style="gap:8px">
                                {qr.into_iter().map(|b| { let b2 = b.clone(); view! { <button class="btn" on:click=move |_| send(json!({"button": b2.clone()}))>{format!("Tap “{b}”")}</button> } }).collect_view()}
                                <button class="btn" on:click=move |_| send(json!({"image": "Here is the receipt"}))>"Send image"</button>
                                <button class="btn" on:click=move |_| send(json!({"location": {"latitude": 40.7484, "longitude": -73.9857, "name": "Home"}}))>"Send location"</button>
                                <span class="grow"></span>
                                <button class="btn primary" disabled=move || draft.get().is_empty() on:click=move |_| send(json!({"text": draft.get_untracked()}))>"Send"</button>
                            </div>
                            <div class="small cap">{move || format!("Sent to the app as a messages webhook{}.", if number.get()["number"]["webhook"]["sign"] == true { ", signed with the app secret" } else { "" })}</div>
                        }
                    })}
                </div>
                <div class="side envside">
                    {move || {
                        let m = sel.get();
                        if m.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick a message to see what the app saw of it."</div> }.into_any(); }
                        let id = s(&m, "id");
                        let (i1, i2) = (id.clone(), id.clone());
                        let ex = explain.get();
                        let num = number.get();
                        view! {
                            <div class="caps">{if s(&m, "template").is_empty() { "The message" } else { "The template message" }}</div>
                            <div class="mono small" style="margin:6px 0 18px;word-break:break-all">{id.clone()}</div>
                            <div class="caps">"Status, as the app saw it"</div>
                            {m["history"].as_array().cloned().unwrap_or_default().into_iter().map(|h| {
                                let wh = h["webhook"].clone();
                                view! { <div class="hrow"><span class="mono small cap">{api::hms(api::n(&h, "at"))}</span><div>
                                    <div class=status_class(&s(&h, "status"))>{format!("{}{}", s(&h, "status"), h["error"].as_i64().map(|e| format!(" · {e} {}", error_name(e))).unwrap_or_default())}</div>
                                    <div class="small cap">{if s(&h, "status") == "accepted" { "200 · wamid returned".to_string() } else if wh.is_null() { "webhook pending".into() } else if wh["status"].is_null() { format!("webhook failed: {}", s(&wh, "error")) } else { format!("webhook → app {}{}", wh["status"], if api::n(&wh, "attempt") > 1 { format!(" (attempt {})", api::n(&wh, "attempt")) } else { String::new() }) }}</div>
                                </div></div> }
                            }).collect_view()}
                            {(s(&m, "dir") == "out").then(|| view! {
                                <div class="caps" style="margin:20px 0 8px">"Next messages to this number"</div>
                                {let num_digits = s(&m, "number"); let next_open = RwSignal::new(false); view! {
                                <span class="anchor"><button class="btn" on:click=move |_| next_open.set(true)>{s(&ex, "next")}<span class="small">" ▾"</span></button>
                                    <Pop open=next_open>
                                        <div class="mh caps">"This number, from now on"</div>
                                        {[("sent → delivered → read", json!([{"delivered_after": "1s"}, {"read_after": "30s"}])),
                                          ("sent → delivered (receipts off)", json!([{"delivered_after": "1s"}, {"never_read": true}])),
                                          ("stays at sent", json!([{"stay_at_sent": true}])),
                                          ("fails with 131049", json!([{"fail": 131049}]))].into_iter().map(|(l, steps)| {
                                            let n = num_digits.clone();
                                            view! { <div class="mi" on:click=move |_| { let (n, steps) = (n.clone(), steps.clone()); leptos::task::spawn_local(async move {
                                                match api::send("PUT", &format!("/whatsapp/recipients/{}", enc(&format!("+{n}"))), json!({"steps": steps})).await {
                                                    Ok(_) => ui::toast(format!("Next messages to +{n}: {l}")), Err(e) => ui::toast_err(e) } }) }>{l}</div> }
                                        }).collect_view()}
                                    </Pop></span> }}
                                <a class="link small" style="display:block;margin-top:6px" href=format!("/whatsapp/recipients?number={}", enc(&s(&ex["match"], "number")))>{format!("From the recipient rule {}; change it in Recipients ›", if s(&ex["match"], "number") == "*" { "“Everyone else”".to_string() } else { s(&ex["match"], "number") })}</a>
                                <div class="caps" style="margin:20px 0 8px">"Provoke on this message"</div>
                                <div class="row" style="gap:8px">
                                    <span class="anchor"><button class="btn" on:click=move |_| fail_open.set(true)>"Fail it"<span class="small">" ▾"</span></button>
                                        <Pop open=fail_open>
                                            <div class="mh caps">"Meta error"</div>
                                            {[131026i64, 131047, 131049, 131051, 130429, 131000].into_iter().map(|c| { let i = i1.clone(); view! { <div class="mi" on:click=move |_| set_status(i.clone(), "failed", Some(c))><span class="mono">{c}</span>" "{error_name(c)}</div> } }).collect_view()}
                                            <div class="rule"></div><div class="mh small cap">"Sends a failed status webhook to the app"</div>
                                        </Pop></span>
                                    <button class="btn" on:click=move |_| resend(i2.clone())>"Send “read” again"</button>
                                </div>
                                <div class="small cap" style="margin-top:6px">"Fail sends a failed status webhook with the error you pick; again sends a duplicate, to test that the app ignores it."</div>
                                <div class="caps" style="margin:20px 0 8px">"Details"</div>
                                <table class="kv2">
                                    <tr><td>"Pricing"</td><td class="mono">{format!("{}  ·  billable", if s(&m, "category").is_empty() { "service".into() } else { s(&m, "category") })}</td></tr>
                                    <tr><td>"Conversation"</td><td class="mono">{m["history"].as_array().and_then(|h| h.iter().find(|e| e["status"] == "sent")).map(|e| format!("{}  ·  opened {}", if s(&m, "category").is_empty() { "service".to_string() } else { s(&m, "category") }, api::hm(api::n(e, "at")))).unwrap_or("not opened (never sent)".into())}</td></tr>
                                    <tr><td>"Sent by"</td><td class="mono">{s(&m, "caller")}</td></tr>
                                    <tr><td>"Callback"</td><td class="mono" style="word-break:break-all">{if s(&num, "webhookUrl").is_empty() { "not set (Number settings)".into() } else { s(&num, "webhookUrl") }}</td></tr>
                                </table>
                            })}
                        }.into_any()
                    }}
                </div>
            </div>
            <Dialog open=hooks_open title="Webhooks the app did not take">
                <div style="min-width:520px">
                    {move || unresolved.get().into_iter().map(|h| view! {
                        <div class="hrow"><span class="mono small cap">{api::hms(api::n(&h, "at"))}</span><div>
                            <div><span>{format!("{} webhook  ·  ", s(&h, "kind"))}</span><span class="danger">{h["status"].as_i64().map(|c| format!("answered {c}")).unwrap_or(s(&h, "error"))}</span><span class="cap">{format!("  ·  attempt {}", api::n(&h, "attempt"))}</span></div>
                            <div class="mono small cap">{s(&h, "url")}</div></div></div> }).collect_view()}
                </div>
            </Dialog>
        </div>
    }
}
