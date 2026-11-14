//! E2 · Inboxes › E-mail: kept and temporary inboxes, what happens on arrival, the steps that play
//! the recipient, and the New inbox dialog. E2b: the field options. Changes save as you make them.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui::{self, Confirm, Pop, Toggle},
};

fn expires_text(i: &Value) -> (String, bool) {
    match i["expiresAt"].as_i64() {
        None => (String::new(), false),
        Some(e) if e < api::now() as i64 => (format!("expired {} ago", api::span(api::now() as i64 - e)), false),
        Some(e) => (format!("expires in {}", api::span(e - api::now() as i64)), e - (api::now() as i64) < 30 * 60_000),
    }
}

fn who(by: &str) -> String {
    match by {
        "you" => "you".into(),
        b if b.starts_with("mcp:") => b[4..].to_string(),
        b if b.starts_with("api:") => b[4..].to_string(),
        b => b.to_string(),
    }
}

#[component]
pub fn Inboxes() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let selected = Memo::new(move |_| params.get().get("address").unwrap_or_default());
    let list = RwSignal::new(Vec::<Value>::new());
    let domain = RwSignal::new(String::new());
    let q = RwSignal::new(String::new());
    let creating = RwSignal::new(false);
    let prefill = RwSignal::new(String::new());
    let clock = RwSignal::new(0u32);
    set_interval(move || clock.update(|c| *c += 1), std::time::Duration::from_secs(20));

    let load = move || {
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/inboxes").await {
                list.set(v["inboxes"].as_array().cloned().unwrap_or_default());
                domain.set(s(&v, "domain"));
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && (topic == "email.inbox" || topic == "email.message") {
            load();
        }
        t
    });
    let envname = move || api::s(&env.get(), "env").to_uppercase();
    let nav_row = nav.clone();
    let row = move |i: Value| {
        let a = s(&i, "address");
        let a2 = a.clone();
        let nav = nav_row.clone();
        let (exp, soon) = expires_text(&i);
        let n = api::n(&i, "messages");
        let msgs = if n == 0 { "no messages yet".to_string() } else { format!("{n} message{}", if n == 1 { "" } else { "s" }) };
        let sub = if s(&i, "kind") == "kept" {
            let first = if !s(&i, "label").is_empty() { s(&i, "label") } else if s(&i, "onArrival") != "accept" { format!("rule: always {}", s(&i, "onArrival")) } else { "kept".into() };
            format!("{first}  ·  {msgs}")
        } else {
            format!("by {}  ·  {msgs}  ·  {exp}", who(&s(&i, "createdBy")))
        };
        let expired = exp.starts_with("expired");
        view! {
            <div class="irow" class:on=move || selected.get() == a2 class:expired=expired on:click=move |_| nav(&format!("/inboxes/{}", api::enc(&a)), Default::default())>
                <div class="a">{s(&i, "address")}</div>
                <div class="s" class:attn=soon>{sub}</div>
            </div>
        }
    };
    let row = StoredValue::new(row);
    let nav_new = nav.clone();

    view! {
        <div class="page">
            <super::area::EmailHead active="inboxes" right=move || view! {
                <button class="btn primary" on:click=move |_| { prefill.set(String::new()); creating.set(true) }>"New inbox"</button>
            }/>
            <div class="body">
                <div class="list wide">
                    <div style="padding:16px 24px 4px"><input class="input" style="width:100%" placeholder="Find an address" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/></div>
                    {move || {
                        let t = q.get().to_lowercase();
                        let all: Vec<Value> = list.get().into_iter().filter(|i| t.is_empty() || s(i, "address").contains(&t) || s(i, "label").to_lowercase().contains(&t)).collect();
                        let _ = clock.get();
                        if all.is_empty() && !t.is_empty() {
                            let candidate = if t.contains('@') { t.clone() } else { format!("{}@{}", t.split_whitespace().collect::<String>(), domain.get()) };
                            let c2 = candidate.clone();
                            return view! {
                                <div class="empty">
                                    <div>{format!("No inbox matches “{}”.", q.get())}</div>
                                    <button class="link" style="margin-top:8px" on:click=move |_| { prefill.set(c2.clone()); creating.set(true) }>{format!("Create {candidate}")}</button>
                                </div>
                            }.into_any();
                        }
                        let kept: Vec<Value> = all.iter().filter(|i| s(i, "kind") == "kept").cloned().collect();
                        let temp: Vec<Value> = all.iter().filter(|i| s(i, "kind") != "kept").cloned().collect();
                        view! {
                            <div class="section caps">{move || format!("Kept with {}", envname())}</div>
                            {if kept.is_empty() { view! { <div class="note">"None. Keep an inbox to have it survive and never expire."</div> }.into_any() }
                             else { kept.into_iter().map(|i| row.with_value(|r| r(i))).collect_view().into_any() }}
                            <div class="section caps">"Temporary  ·  expire by themselves"</div>
                            {if temp.is_empty() { view! { <div class="note"><div style="color:var(--ink)">"No temporary inboxes"</div>"Tests create them with create_inbox() or POST /inboxes."</div> }.into_any() }
                             else { temp.into_iter().map(|i| row.with_value(|r| r(i))).collect_view().into_any() }}
                        }.into_any()
                    }}
                    <div class="note">{move || format!("Any address at {} is accepted even before it exists, so a test can invent one. Mail to other domains is caught too and never reaches a real inbox.", domain.get())}</div>
                </div>
                <div class="pane pad">
                    {move || {
                        let a = selected.get();
                        if a.is_empty() {
                            view! { <div class="empty"><h3>"Pick an inbox"</h3><div>"Or create one: tests usually make their own temporary inbox and let it expire."</div></div> }.into_any()
                        } else {
                            view! { <InboxPane address=a/> }.into_any()
                        }
                    }}
                </div>
            </div>
            <NewInbox open=creating domain=domain prefill=prefill on_done=move |a: String| nav_new(&format!("/inboxes/{}", api::enc(&a)), Default::default())/>
        </div>
    }
}

const STEP_ACTIONS: [(&str, &str); 4] = [("delivered", "Delivered"), ("opened", "Opened"), ("spamreport", "Spam report"), ("unsubscribe", "Unsubscribe")];
const UNITS: [(&str, u64); 4] = [("ms", 1), ("sec", 1000), ("min", 60_000), ("h", 3_600_000)];

fn unit_ms(u: &str) -> u64 {
    UNITS.iter().find(|x| x.0 == u).map(|x| x.1).unwrap_or(1000)
}

#[component]
fn InboxPane(address: String) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let inbox = RwSignal::new(Value::Null);
    let msgs = RwSignal::new(Vec::<Value>::new());
    let svc = RwSignal::new(Value::Null);
    let extractors = RwSignal::new(Vec::<String>::new());
    let err = RwSignal::new(String::new());
    let extend_open = RwSignal::new(false);
    let confirm = RwSignal::new(false);
    let addr = StoredValue::new(address.clone());
    let clock = RwSignal::new(0u32);
    set_interval(move || clock.update(|c| *c += 1), std::time::Duration::from_secs(15));

    let fetch = move || {
        let a = addr.get_value();
        leptos::task::spawn_local(async move {
            match api::get(&format!("/inboxes/{}", api::enc(&a))).await {
                Ok(v) => {
                    inbox.set(v["inbox"].clone());
                    msgs.set(v["messages"].as_array().cloned().unwrap_or_default());
                    err.set(String::new());
                }
                Err(e) => err.set(e),
            }
        });
    };
    fetch();
    leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/services").await {
            svc.set(v["services"][0]["service"].clone());
        }
        if let Ok(v) = api::get("/channels/email/extractors").await {
            extractors.set(v["yours"].as_array().map(|a| a.iter().map(|e| s(&e["extractor"], "name")).collect()).unwrap_or_default());
        }
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let last = live.last.get_untracked();
        let topic = s(&last, "topic");
        if prev.is_some() && (topic == "email.message" || (topic == "email.inbox" && s(&last, "id") == addr.get_value())) {
            fetch();
        }
        t
    });

    // Every change saves at once; the server validates (order rule, unknown extractor…).
    let patch = move |body: Value| {
        let a = addr.get_value();
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/inboxes/{}", api::enc(&a)), body).await {
                Ok(v) => {
                    inbox.set(v);
                    err.set(String::new());
                }
                Err(e) => err.set(e),
            }
        });
    };
    let post = move |path: &'static str, body: Value| {
        let a = addr.get_value();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/inboxes/{}/{path}", api::enc(&a)), body).await {
                Ok(v) => inbox.set(v),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let nav_del = nav.clone();
    let delete = move || {
        let a = addr.get_value();
        let nav = nav_del.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/inboxes/{}", api::enc(&a)), json!({})).await {
                Ok(_) => {
                    ui::toast("Inbox deleted");
                    nav("/inboxes", Default::default());
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };

    // Steps as edited on screen: the inbox's own, or a copy of the service defaults.
    let steps = move || -> Vec<Value> {
        let i = inbox.get();
        match i["steps"].as_array() {
            Some(a) => a.clone(),
            None => svc.get()["events"]["defaultSteps"].as_array().cloned().unwrap_or_default().iter().map(|t| parse_step(t.as_str().unwrap_or(""))).collect(),
        }
    };
    let save_steps = move |v: Vec<Value>| patch(json!({"steps": v}));
    let set_step = move |idx: usize, k: &'static str, val: Value| {
        let mut v = steps();
        if let Some(st) = v.get_mut(idx) {
            st[k] = val;
        }
        save_steps(v);
    };

    view! {
        {move || (!err.get().is_empty() && inbox.get().is_null()).then(|| view! { <div class="empty"><h3>{err.get()}</h3></div> })}
        {move || {
            let i = inbox.get();
            if i.is_null() {
                return view! { <div class="cap">"Loading…"</div> }.into_any();
            }
            let _ = clock.get();
            let kept = s(&i, "kind") == "kept";
            let created = format!("created by {} at {}", match s(&i, "createdBy").as_str() { "you" => "you".to_string(), b if b.starts_with("mcp:") => format!("{} (MCP)", &b[4..]), b if b.starts_with("api:") => format!("{} (API)", &b[4..]), b => b.to_string() }, api::hm(api::n(&i, "createdAt")));
            let exp = i["expiresAt"].as_i64().map(|e| if e < api::now() as i64 { format!("expired at {}, {} ago  ·  mail sent here now lands in the catch-all", api::hm(e), api::span(api::now() as i64 - e)) } else { format!("expires at {}, in {}", api::hm(e), api::span(e - api::now() as i64)) });
            let arrival = s(&i, "onArrival");
            let steps_off = arrival == "bounce" || arrival == "drop";
            let own = i["steps"].is_array();
            let a_copy = s(&i, "address");
            view! {
                <div class="mhead">
                    <div>
                        <div class="row"><span class="mono" style="font-size:20px">{s(&i, "address")}</span>
                            <button class="link plain" on:click=move |_| ui::copy(&a_copy, "Address")>"Copy"</button></div>
                        <div class="meta" style="margin-top:6px">{if kept { format!("Kept with {}  ·  never expires, survives clones  ·  {created}", api::s(&env.get(), "env").to_uppercase()) } else { format!("Temporary  ·  {created}  ·  {}", exp.unwrap_or_default()) }}</div>
                    </div>
                    <div class="row">
                        {(!kept).then(|| view! {
                            <div class="anchor">
                                <button class="btn" on:click=move |_| extend_open.set(true)>"Extend ▾"</button>
                                <Pop open=extend_open right=true>
                                    {[("15m", "+ 15 min"), ("30m", "+ 30 min"), ("2h", "+ 2 hours"), ("24h", "+ 24 hours")].into_iter().map(|(by, l)| view! {
                                        <div class="mi" on:click=move |_| post("extend", json!({"by": by}))>{l}</div>
                                    }).collect_view()}
                                    <div class="rule"></div>
                                    <div class="mi" on:click=move |_| post("keep", json!({}))><span>{move || format!("Keep with {}", api::s(&env.get(), "env").to_uppercase())}</span><span class="sub">"never expires"</span></div>
                                </Pop>
                            </div>
                        })}
                        <button class="btn danger" on:click=move |_| confirm.set(true)>"Delete"</button>
                    </div>
                </div>

                <div class="block">
                    <h3 class="h">"When a message arrives"</h3>
                    {[("accept", "Accept it", String::new()), ("bounce", "Bounce it", String::new()), ("defer", "Defer it", String::new()), ("hold", "Hold it", "decide by hand, or let the steps below decide".to_string()), ("drop", "Drop it", "the provider refuses to send (dropped)".to_string())]
                        .into_iter().map(|(k, label, hint)| {
                            let on = arrival == k;
                            view! {
                                <div class="radio" class:on=on on:click=move |_| if !on { patch(json!({"onArrival": k})) }>
                                    <span class="r"></span><span>{label}</span>
                                    {match k {
                                        "bounce" => view! { <span class="t2">"  ·  "</span>
                                            <select class="select" style="height:28px" on:click=|e| e.stop_propagation() on:change=move |e| patch(json!({"bounceKind": event_target_value(&e), "onArrival": "bounce"}))>
                                                <option value="hard" selected=s(&i, "bounceKind") == "hard">"hard bounce, 550 mailbox unavailable"</option>
                                                <option value="soft" selected=s(&i, "bounceKind") == "soft">"soft bounce, 452 mailbox full"</option>
                                            </select> }.into_any(),
                                        "defer" => view! {
                                            <input class="input mono" style="width:52px;height:28px" type="number" min="1" max="20" prop:value=api::n(&i, "deferTimes").to_string()
                                                on:click=|e| e.stop_propagation() on:change=move |e| patch(json!({"deferTimes": event_target_value(&e).parse::<u64>().unwrap_or(3), "onArrival": "defer"}))/>
                                            <span>"times, then accept  ·  "</span><span class="t2">"tests the app's retry"</span> }.into_any(),
                                        "hold" if on => view! { <span class="t2">"·  if nobody decides in"</span>
                                            <select class="select" style="height:28px" on:click=|e| e.stop_propagation() on:change=move |e| patch(json!({"holdFor": event_target_value(&e)}))>
                                                {[("1m", "1 min"), ("5m", "5 min"), ("10m", "10 min"), ("30m", "30 min"), ("1h", "1 hour")].into_iter().map(|(v, l)| view! { <option value=v selected=api::n(&i, "holdForMs") as u64 == duration(v)>{l}</option> }).collect_view()}
                                            </select><span class="t2">"…the steps decide"</span> }.into_any(),
                                        _ => view! { <span class="t2">{if hint.is_empty() { String::new() } else { format!("·  {hint}") }}</span> }.into_any(),
                                    }}
                                </div>
                            }
                        }).collect_view()}
                    {(arrival == "hold").then(|| view! { <div class="small t2" style="margin-top:6px">"Held messages show Deliver, Defer, Bounce and Drop in Notifications, and over MCP as decide_held()."</div> })}
                </div>

                <div class="block">
                    <h3 class="h">"Then simulate automatically"</h3>
                    <div class="small t2">{if steps_off { "Steps are off: every message is refused before delivery, so nothing can be opened or clicked. They come back when you choose Accept, Defer or Hold.".to_string() } else { "Steps run on every message that lands here, so a test never waits for a person.".to_string() }}</div>
                    {(!own && !steps_off).then(|| view! { <div class="small cap" style="margin-top:4px">"Using the service defaults. Any change here gives this inbox its own steps."</div> })}
                    <div class="steps" style:opacity=if steps_off { "0.45" } else { "1" } style:pointer-events=if steps_off { "none" } else { "auto" }>
                        <span></span><span class="caps">"Step"</span><span class="caps">"Wait"</span><span></span><span class="caps">"Counted from"</span><span class="caps">"Vary by"</span><span></span>
                        {steps().into_iter().enumerate().map(|(idx, st)| {
                            let on = st["on"] != false;
                            let unit = s(&st, "unit");
                            let unit = if unit.is_empty() { "sec".to_string() } else { unit };
                            let um = unit_ms(&unit);
                            let action = s(&st, "action");
                            let link = s(&st, "link");
                            let cur = if action == "click" { format!("click:{}", if link.is_empty() { "first".into() } else { link.clone() }) } else { action.clone() };
                            let from = s(&st, "from");
                            let from = if from.is_empty() { if action == "delivered" { "arrival".to_string() } else { "previous".to_string() } } else { from };
                            let n_steps = steps().len();
                            view! {
                                <Toggle on=Signal::derive(move || on) set=move |v: bool| set_step(idx, "on", json!(v))/>
                                <select class="select" on:change=move |e| {
                                    let v = event_target_value(&e);
                                    let mut all = steps();
                                    if let Some(x) = all.get_mut(idx) {
                                        if let Some(l) = v.strip_prefix("click:") { x["action"] = json!("click"); x["link"] = json!(l); } else { x["action"] = json!(v); x["link"] = Value::Null; }
                                    }
                                    save_steps(all);
                                }>
                                    {STEP_ACTIONS.iter().map(|(v, l)| view! { <option value=*v selected=cur == *v>{*l}</option> }).collect_view()}
                                    <optgroup label="Click a link">
                                        <option value="click:any" selected=cur == "click:any">"Any link, at random"</option>
                                        <option value="click:first" selected=cur == "click:first">"First link in the body"</option>
                                    </optgroup>
                                    <optgroup label="From an extractor">
                                        {extractors.get().into_iter().map(|x| { let v = format!("click:{x}"); view! { <option value=v.clone() selected=cur == v>{format!("Click {x}")}</option> } }).collect_view()}
                                    </optgroup>
                                </select>
                                <input class="input mono" type="number" min="0" prop:value=(st["waitMs"].as_u64().unwrap_or(0) / um).to_string()
                                    on:change=move |e| { let n = event_target_value(&e).parse::<u64>().ok().filter(|n| n * um <= 86_400_000); if let Some(n) = n { set_step(idx, "waitMs", json!(n * um)) } else { err.set("Use a number from 0 to 86,400.".into()) } }/>
                                <select class="select" on:change=move |e| {
                                    let u = event_target_value(&e);
                                    let mut all = steps();
                                    if let Some(x) = all.get_mut(idx) { let n = x["waitMs"].as_u64().unwrap_or(0) / um; x["waitMs"] = json!(n * unit_ms(&u)); x["unit"] = json!(u); }
                                    save_steps(all);
                                }>
                                    {UNITS.iter().map(|(u, _)| view! { <option value=*u selected=unit == *u>{*u}</option> }).collect_view()}
                                </select>
                                <select class="select" on:change=move |e| set_step(idx, "from", json!(event_target_value(&e)))>
                                    <option value="arrival" selected=from == "arrival">"arrival"</option>
                                    <option value="previous" selected=from == "previous">"previous step"</option>
                                    <option value="delivery" selected=from == "delivery">"delivery"</option>
                                    {(1..=n_steps).filter(|n| *n != idx + 1).map(|n| { let v = format!("step:{n}"); view! { <option value=v.clone() selected=from == v>{format!("step {n}")}</option> } }).collect_view()}
                                </select>
                                <div class="row" style="gap:4px"><span class="cap">"±"</span>
                                    <input class="input mono" style="width:64px" type="number" min="0" prop:value=(st["varyMs"].as_u64().unwrap_or(0) / um).to_string()
                                        on:change=move |e| set_step(idx, "varyMs", json!(event_target_value(&e).parse::<u64>().unwrap_or(0) * um))/></div>
                                <button class="link plain" style="text-align:left" on:click=move |_| { let mut all = steps(); all.remove(idx); save_steps(all); }>"Remove"</button>
                            }
                        }).collect_view()}
                    </div>
                    <div class="row" style="margin-top:12px">
                        <button class="link plain" style="font-size:13px" on:click=move |_| { let mut all = steps(); all.push(json!({"on": true, "action": "opened", "waitMs": 30000, "unit": "sec", "from": "previous", "varyMs": 0})); save_steps(all); }>"+  Add step"</button>
                        <span class="grow"></span>
                        <span class="t2">"Apply to"</span>
                        <ApplyTo value=s(&i, "applyTo") extractors=extractors.get() set=move |v: String| patch(json!({"applyTo": v}))/>
                    </div>
                    {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{format!("ⓘ  {}", err.get())}</div> })}
                </div>

                <div class="block">
                    <div class="row"><h3 class="h">"Messages"</h3><span class="t2">{move || msgs.get().len()}</span><span class="grow"></span>
                        <a class="link" href=format!("/notifications?to={}", api::enc(&s(&i, "address")))>"Open in Notifications →"</a></div>
                    {move || {
                        let m = msgs.get();
                        if m.is_empty() {
                            return view! { <div class="small cap" style="margin-top:8px">"No messages yet. Send one to this address."</div> }.into_any();
                        }
                        m.into_iter().map(|m| {
                            let id = s(&m, "id");
                            let extra = m["extracted"].as_object().and_then(|o| o.iter().find(|(_, v)| v["by"] == "yours").map(|(k, v)| { let val = s(v, "value"); if val.starts_with("http") { k.clone() } else { format!("{k} {val}") } })).unwrap_or(s(&m, "lang"));
                            let st = s(&m, "status");
                            let mark = if m["provoked"] == true || s(&m, "statusBy") == "step" { format!("◆ {st} by {}", if s(&m, "statusBy") == "step" { "step" } else { "rule" }) } else { st };
                            view! {
                                <a class="msg-line" href=format!("/notifications/{id}")>
                                    <span style="color:var(--ink)">{s(&m, "subject")}</span>
                                    <span class="r">{format!("{extra}  ·  {mark}  ·  {}", api::hm(api::n(&m, "at")))}</span>
                                </a>
                            }
                        }).collect_view().into_any()
                    }}
                </div>
                <Confirm open=confirm question=Signal::derive(move || format!("Delete this inbox and its {} messages?", msgs.get().len()))
                    detail="Mail sent here afterwards still lands in the catch-all." action="Delete" on_yes={ let d = delete.clone(); move |_| d() }/>
            }.into_any()
        }}
    }
}

fn duration(v: &str) -> u64 {
    let n: u64 = v.trim_end_matches(|c: char| c.is_alphabetic()).parse().unwrap_or(0);
    n * if v.ends_with('h') { 3_600_000 } else { 60_000 }
}

/// "delivered +1s" → the object the screen edits.
fn parse_step(t: &str) -> Value {
    let mut parts = t.split_whitespace();
    let head = parts.next().unwrap_or("delivered");
    let (action, link) = head.split_once(':').map(|(a, l)| (a.to_string(), Some(l.to_string()))).unwrap_or((head.to_string(), None));
    let mut v = json!({"on": true, "action": action, "link": link, "waitMs": 0, "unit": "sec", "from": if action == "delivered" { "arrival" } else { "previous" }, "varyMs": 0});
    let ms = |x: &str| -> (u64, &'static str) {
        let n: f64 = x.trim_end_matches(|c: char| c.is_alphabetic()).parse().unwrap_or(0.0);
        if x.ends_with("ms") { (n as u64, "ms") } else if x.ends_with('m') { ((n * 60_000.0) as u64, "min") } else if x.ends_with('h') { ((n * 3_600_000.0) as u64, "h") } else { ((n * 1000.0) as u64, "sec") }
    };
    for p in parts {
        if let Some(w) = p.strip_prefix('+') {
            let (n, u) = ms(w);
            v["waitMs"] = json!(n);
            v["unit"] = json!(u);
        } else if let Some(w) = p.strip_prefix('±') {
            v["varyMs"] = json!(ms(w).0);
        }
    }
    v
}

#[component]
fn ApplyTo(value: String, extractors: Vec<String>, #[prop(into)] set: Callback<String>) -> impl IntoView {
    let (kind, arg) = value.split_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((value.clone(), String::new()));
    let kind = RwSignal::new(kind);
    let arg = RwSignal::new(arg);
    let commit = move || {
        let k = kind.get_untracked();
        let a = arg.get_untracked();
        if matches!(k.as_str(), "every" | "first") {
            set.run(k);
        } else if !a.trim().is_empty() {
            set.run(format!("{k}:{}", a.trim()));
        }
    };
    let opts = [("every", "Every message"), ("first", "Only the first message"), ("first_n", "The first N messages…"), ("subject", "Subject matches…"), ("kind", "Kind is…"), ("has", "Has a value from an extractor…"), ("from", "Sent from…")];
    view! {
        <select class="select" style="width:220px" on:change=move |e| { kind.set(event_target_value(&e)); arg.set(String::new()); commit(); }>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || kind.get() == v>{l}</option> }).collect_view()}
        </select>
        {move || match kind.get().as_str() {
            "every" | "first" => ().into_any(),
            "has" => view! {
                <select class="select" on:change=move |e| { arg.set(event_target_value(&e)); commit(); }>
                    <option value="">"pick one"</option>
                    {extractors.iter().cloned().map(|x| { let (x1, x2) = (x.clone(), x.clone()); view! { <option value=x1 selected=move || arg.get() == x2>{x}</option> } }).collect_view()}
                </select>
            }.into_any(),
            k => view! {
                <input class="input" style="width:160px" placeholder=match k { "first_n" => "3", "subject" => "Action required|Acción", "kind" => "Your verification code", _ => "no-reply@acme.example.com" }
                    prop:value=move || arg.get() on:input=move |e| arg.set(event_target_value(&e)) on:change=move |_| commit()/>
            }.into_any(),
        }}
    }
}

#[component]
fn NewInbox(open: RwSignal<bool>, domain: RwSignal<String>, prefill: RwSignal<String>, #[prop(into)] on_done: Callback<String>) -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let mode = RwSignal::new("random".to_string());
    let local = RwSignal::new(String::new());
    let random = RwSignal::new(String::new());
    let kept = RwSignal::new(false);
    let ttl = RwSignal::new("30m".to_string());
    let arrival = RwSignal::new("accept".to_string());
    let steps = RwSignal::new("default".to_string());
    let err = RwSignal::new(String::new());
    let new_random = move || random.set(format!("signer-{:04x}", (js_sys::Math::random() * 65535.0) as u32));
    Effect::new(move |_| {
        if open.get() {
            new_random();
            err.set(String::new());
            let p = prefill.get_untracked();
            if p.is_empty() {
                mode.set("random".into());
            } else {
                mode.set("choose".into());
                local.set(p.split('@').next().unwrap_or("").to_string());
            }
        }
    });
    let address = move || {
        let lp = if mode.get() == "random" { random.get() } else { local.get().trim().to_lowercase() };
        let p = prefill.get();
        if mode.get() == "choose" && p.contains('@') && p.starts_with(&lp) && !p.ends_with(&domain.get()) { p } else { format!("{lp}@{}", domain.get()) }
    };
    let create = move |_| {
        let a = address();
        let body = json!({"address": a, "keep": kept.get_untracked(), "ttl": ttl.get_untracked(), "onArrival": arrival.get_untracked(), "steps": steps.get_untracked()});
        leptos::task::spawn_local(async move {
            match api::send("POST", "/inboxes", body).await {
                Ok(v) => {
                    let a = s(&v, "address");
                    if let Some(w) = web_sys::window() {
                        let _ = w.navigator().clipboard().write_text(&a);
                    }
                    ui::toast(format!("{a} created and copied"));
                    open.set(false);
                    on_done.run(a);
                }
                Err(e) => err.set(e),
            }
        });
    };
    view! {
        <ui::Dialog open=open title="New inbox">
            <div class="field">
                <label>"Address"</label>
                <ui::Seg options=vec![("random", "Random"), ("choose", "Choose")] value=mode set=move |v| mode.set(v)/>
                {move || if mode.get() == "random" {
                    view! { <div class="row"><span class="mono">{address()}</span><button class="link" on:click=move |_| new_random()>"New random part"</button></div> }.into_any()
                } else {
                    view! { <div class="row"><input class="input mono" style="width:180px" prop:value=move || local.get() on:input=move |e| local.set(event_target_value(&e))/><span class="mono t2">{move || format!("@{}", domain.get())}</span></div> }.into_any()
                }}
            </div>
            <div class="field" style="margin-top:16px">
                <label>"Lifetime"</label>
                <div class="radio" class:on=move || !kept.get() on:click=move |_| kept.set(false)><span class="r"></span>"Temporary  ·  expires after"
                    <select class="select" style="height:28px" on:change=move |e| ttl.set(event_target_value(&e))>
                        {[("15m", "15 min"), ("30m", "30 min"), ("2h", "2 hours"), ("24h", "24 hours")].into_iter().map(|(v, l)| view! { <option value=v selected=move || ttl.get() == v>{l}</option> }).collect_view()}
                    </select></div>
                <div class="radio" class:on=move || kept.get() on:click=move |_| kept.set(true)><span class="r"></span>{move || format!("Kept with {}  ·  never expires, survives clones", api::s(&env.get(), "env").to_uppercase())}</div>
            </div>
            <div class="grid2" style="margin-top:16px">
                <div class="field"><label>"When a message arrives"</label>
                    <select class="select" on:change=move |e| arrival.set(event_target_value(&e))>
                        {[("accept", "Accept it"), ("bounce", "Bounce it"), ("defer", "Defer it 3 times"), ("hold", "Hold it"), ("drop", "Drop it")].into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}
                    </select></div>
                <div class="field"><label>"Then simulate automatically"</label>
                    <select class="select" on:change=move |e| steps.set(event_target_value(&e))>
                        <option value="default">"Service defaults"</option>
                        <option value="none">"No steps"</option>
                    </select></div>
            </div>
            <div class="small cap" style="margin-top:8px">"Edit the steps after creating. The address is copied when you create it."</div>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
            <div class="actions">
                <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button>
                <button class="btn primary" on:click=create>"Create inbox"</button>
            </div>
        </ui::Dialog>
    }
}
