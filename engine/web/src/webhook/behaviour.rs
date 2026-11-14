//! H2 · Inboxes › Behaviour: how an inbox answers (mode, errors, timing, traps, signature
//! strictness, lifetime), with its last event's attempts and its YAML beside.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use super::{gap, Head, InboxList};
use crate::{
    api::{self, s},
    ui::{self, Confirm, Toggle},
};

const MODES: [(&str, &str); 8] = [
    ("always_200", "Always 200"),
    ("accept_202", "Accept with 202"),
    ("error", "Always an error…"),
    ("fail_then", "Fail N times, then succeed"),
    ("fail_share", "Fail a share of deliveries"),
    ("hang", "Hang (no answer)"),
    ("reset", "Reset the connection"),
    ("empty", "Empty reply"),
];
const ERRORS: [u16; 8] = [400, 401, 404, 409, 429, 500, 502, 503];
const OKS: [u16; 3] = [200, 202, 204];

#[component]
pub fn Behaviour() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let sh = super::shared();
    let name = Memo::new(move |_| params.get().get("name").unwrap_or_default());
    let info = RwSignal::new(Value::Null);
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let life = RwSignal::new(String::new());
    let err = RwSignal::new(String::new());
    let confirm = RwSignal::new(false);

    let load = move |keep_draft: bool| {
        let n = name.get_untracked();
        if n.is_empty() {
            return;
        }
        leptos::task::spawn_local(async move {
            match api::get(&format!("/webhooks/inboxes/{}", api::enc(&n))).await {
                Ok(v) => {
                    let dirty = draft.get_untracked() != saved.get_untracked();
                    saved.set(v["inbox"].clone());
                    if !(keep_draft && dirty) {
                        draft.set(v["inbox"].clone());
                        life.set(if v["inbox"]["kept"] == true { "kept".into() } else { "same".into() });
                    }
                    info.set(v);
                    err.set(String::new());
                }
                Err(e) => err.set(e),
            }
        });
    };
    Effect::new(move |_| {
        name.track();
        load(false);
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("webhook.") {
            load(true);
        }
        t
    });

    let changes = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        let mut n = ["answer", "timing", "traps", "signature"].iter().map(|k| {
            match (&a[*k], &b[*k]) {
                (Value::Object(x), Value::Object(y)) => x.keys().chain(y.keys()).collect::<std::collections::BTreeSet<_>>().into_iter().filter(|kk| x.get(*kk) != y.get(*kk)).count(),
                (x, y) => (x != y) as usize,
            }
        }).sum::<usize>();
        if life.get() != "same" && life.get() != if a["kept"] == true { "kept" } else { "same" } {
            n += 1;
        }
        n
    });
    let set = move |path: &'static str, v: Value| draft.update(|d| match d.pointer_mut(path) {
        Some(slot) => *slot = v,
        None if path.matches('/').count() == 1 => d[&path[1..]] = v,
        None => {}
    });
    let val = move |path: &'static str| draft.get().pointer(path).cloned().unwrap_or(Value::Null);

    let save = move |_| {
        let n = name.get_untracked();
        let l = life.get_untracked();
        let mut body = json!({"inbox": draft.get_untracked()});
        if l != "same" {
            body["expires"] = json!(if l == "kept" { "never".to_string() } else { l });
        }
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/webhooks/inboxes/{}", api::enc(&n)), body).await {
                Ok(_) => { ui::toast("Saved"); load(false); }
                Err(e) => err.set(e),
            }
        });
    };
    let nav_rm = nav.clone();
    let remove = move |_| {
        let n = name.get_untracked();
        let nav = nav_rm.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/webhooks/inboxes/{}", api::enc(&n)), json!({})).await {
                Ok(_) => { ui::toast(format!("{n} removed")); nav("/inboxes/webhooks", Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let right = move || view! {
        {move || { let c = changes.get(); (c > 0).then(|| view! {
            <span class="small attn">{format!("{c} unsaved change{}", if c == 1 { "" } else { "s" })}</span>
            <button class="btn" on:click=move |_| { draft.set(saved.get_untracked()); life.set(if saved.get_untracked()["kept"] == true { "kept".into() } else { "same".into() }); }>"Discard"</button>
        }) }}
        <button class="btn primary" disabled=move || changes.get() == 0 on:click=save>"Save"</button>
    };
    let row_label = |l: &'static str| view! { <span style="width:130px;flex:none">{l}</span> };

    view! {
        <div class="page">
            <Head active="behaviour" name=name right=right sh=sh/>
            <div class="body">
                <InboxList sh=sh selected=name tab="behaviour"/>
                <div class="pane pad form">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">{move || if err.get().is_empty() { "Loading…".to_string() } else { err.get() }}</div> }.into_any(); }
                        let mode = s(&d["answer"], "mode");
                        view! {
                            <div class="mhead">
                                <div class="row" style="align-items:baseline"><h2 class="mono" style="font-weight:500;margin:0">{s(&d, "name")}</h2>
                                    <span class="mono small cap">{move || s(&info.get(), "url")}</span></div>
                            </div>

                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Answer"</div>
                                <div class="row" style="margin-bottom:10px">{row_label("Mode")}
                                    <select class="select" on:change=move |e| set("/answer/mode", json!(event_target_value(&e)))>
                                        {MODES.iter().map(|(v, l)| view! { <option value=*v selected=mode == *v>{*l}</option> }).collect_view()}
                                    </select></div>
                                {matches!(mode.as_str(), "error" | "fail_then" | "fail_share").then(|| view! {
                                    <div class="row" style="margin-bottom:10px">{row_label(if mode == "error" { "Answer" } else { "Fail with" })}
                                        <select class="select" on:change=move |e| set("/answer/status", json!(event_target_value(&e).parse::<u16>().unwrap_or(500)))>
                                            {ERRORS.iter().map(|c| view! { <option value=c.to_string() selected=val("/answer/status") == json!(c)>{c.to_string()}</option> }).collect_view()}
                                        </select>
                                        {(mode == "fail_then").then(|| view! { <span class="t2">"times"</span>
                                            <input class="input mono" style="width:64px" type="number" min="1" max="50" prop:value=val("/answer/times").to_string() on:change=move |e| set("/answer/times", json!(event_target_value(&e).parse::<u32>().unwrap_or(3)))/> })}
                                        {(mode == "fail_share").then(|| view! {
                                            <input class="input mono" style="width:64px" type="number" min="0" max="100" prop:value=val("/answer/share").to_string() on:change=move |e| set("/answer/share", json!(event_target_value(&e).parse::<u32>().unwrap_or(50)))/>
                                            <span class="t2">"% of deliveries"</span> })}
                                    </div>
                                })}
                                {matches!(mode.as_str(), "fail_then" | "fail_share").then(|| view! {
                                    <div class="row" style="margin-bottom:10px">{row_label("Then")}
                                        <select class="select" on:change=move |e| set("/answer/then", json!(event_target_value(&e).parse::<u16>().unwrap_or(200)))>
                                            {OKS.iter().map(|c| view! { <option value=c.to_string() selected=val("/answer/then") == json!(c)>{c.to_string()}</option> }).collect_view()}
                                        </select></div>
                                })}
                                {(mode == "fail_then").then(|| view! {
                                    <div class="row"><Toggle on=Signal::derive(move || val("/answer/per") == "event_id") set=move |v: bool| set("/answer/per", json!(if v { "event_id" } else { "inbox" }))/>
                                        <span>"Count per event id"</span>
                                        <span class="small t2">{move || if val("/answer/per") == "event_id" { format!("each event fails {} times, not the inbox", val("/answer/times")) } else { "the first N deliveries to the inbox fail, whatever the event".to_string() }}</span></div>
                                })}
                                {match mode.as_str() {
                                    "reset" => Some("The TCP connection is reset: the app sees “connection reset by peer”."),
                                    "empty" => Some("The connection closes without a byte: the app sees an empty reply."),
                                    "hang" => Some("No answer until Hang below runs out; the delivery records when the app gave up."),
                                    _ => None,
                                }.map(|t| view! { <div class="small t2">{t}</div> })}
                            </div>

                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Timing"</div>
                                <div class="row" style="margin-bottom:10px">{row_label("Delay")}
                                    <select class="select" on:change=move |e| set("/timing/delay", json!(event_target_value(&e)))>
                                        {[("none", "None"), ("1s", "1 s"), ("2s", "2 s"), ("5s", "5 s"), ("10s", "10 s"), ("31s", "31 s")].into_iter().map(|(v, l)| view! { <option value=v selected=val("/timing/delay") == json!(v)>{l}</option> }).collect_view()}
                                    </select><span class="small t2">"before answering, in every mode"</span></div>
                                <div class="row">{row_label("Hang")}
                                    <select class="select" on:change=move |e| set("/timing/hang", json!(event_target_value(&e)))>
                                        {[("never", "Never"), ("10s", "10 s"), ("35s", "35 s"), ("65s", "65 s"), ("120s", "2 min")].into_iter().map(|(v, l)| view! { <option value=v selected=val("/timing/hang") == json!(v)>{l}</option> }).collect_view()}
                                    </select><span class="small t2">{move || if mode == "hang" { "35 s tests a 30 s client timeout" } else { "used by the Hang mode" }}</span></div>
                            </div>

                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Traps"</div>
                                {[("redirect_private", "Redirect to a private address"), ("huge", "Huge answer (50 MB)"), ("slow", "Slow answer (1 byte per second)")].into_iter().map(|(k, l)| view! {
                                    <div class="row" style="margin-bottom:10px">
                                        <Toggle on=Signal::derive(move || val("/traps").as_array().map(|a| a.iter().any(|x| x == k)).unwrap_or(false))
                                            set=move |on: bool| draft.update(|d| { let a = d["traps"].as_array_mut().unwrap(); a.retain(|x| x != k); if on { a.push(json!(k)); } })/>
                                        <span style="width:240px">{l}</span>
                                        {(k == "redirect_private").then(|| view! {
                                            <button class="link plain mono small" style="color:var(--caption)" title="Click to switch the target: a loopback address Stuntman can see (default), or 10.0.0.5, a customer's network it cannot see"
                                                on:click=move |_| { let cur = s(&draft.get_untracked(), "redirectTo"); set("/redirectTo", json!(if cur.is_empty() { "http://10.0.0.5/admin" } else { "" })); }>
                                                {move || { let t = s(&draft.get(), "redirectTo"); if t.is_empty() { format!("302 → http://127.0.0.1:{}/_trap/…", s(&info.get(), "url").rsplit(':').next().and_then(|x| x.split('/').next()).unwrap_or("")) } else { format!("302 → {t}  ·  not observable") } }}
                                            </button>
                                        })}
                                    </div>
                                }).collect_view()}
                                <div class="small t2">"The app should not follow the redirect, cap what it reads and give up on slow answers. Each trap is recorded as passed or failed on the delivery; with several on, deliveries take turns."</div>
                            </div>

                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Signature"</div>
                                <div class="row" style="margin-bottom:10px">{row_label("Secret")}
                                    {match s(&d["signature"], "secret").strip_prefix("secret://").map(String::from) {
                                        Some(n) => view! { <span class="link" style="font-size:13px">{format!("from Secrets › {n}")}</span>
                                            <span class="small cap">"(data/secrets.yaml until the Secrets service exists)"</span><span class="grow"></span>
                                            <button class="link plain" on:click=move |_| set("/signature/secret", json!(""))>"Change"</button> }.into_any(),
                                        None => view! { <input class="input mono grow" placeholder="secret://name, env:NAME, or the secret itself" prop:value=s(&d["signature"], "secret") on:input=move |e| set("/signature/secret", json!(event_target_value(&e)))/> }.into_any(),
                                    }}</div>
                                {(!s(&saved.get_untracked()["signature"], "previousSecret").is_empty()).then(|| view! {
                                <div class="row" style="margin-bottom:10px">{row_label("Previous secret")}
                                    <input class="input mono grow" placeholder="before rotation: shown as “old secret”" prop:value=s(&d["signature"], "previousSecret") on:input=move |e| set("/signature/previousSecret", json!(event_target_value(&e)))/></div>
                                })}
                                <div class="row" style="margin-bottom:10px">{row_label("Scheme")}
                                    <select class="select" on:change=move |e| set("/signature/scheme", json!(event_target_value(&e)))>
                                        {[("t-v1-hmac-sha256", "t=…,v1=HMAC-SHA256"), ("hmac-sha256-hex", "HMAC-SHA256, hex"), ("hmac-sha256-base64", "HMAC-SHA256, base64"), ("none", "Not signed")].into_iter().map(|(v, l)| view! { <option value=v selected=val("/signature/scheme") == json!(v)>{l}</option> }).collect_view()}
                                    </select>
                                    <span class="t2">"header"</span>
                                    <input class="input mono" style="width:190px" placeholder="any “…Signature…”" prop:value=s(&d["signature"], "header") on:input=move |e| set("/signature/header", json!(event_target_value(&e)))/></div>
                                <div class="row"><Toggle on=Signal::derive(move || val("/signature/rejectBad") == true) set=move |v: bool| set("/signature/rejectBad", json!(v))/>
                                    <span>"Answer 401 to bad signatures"</span><span class="small t2">{move || if val("/signature/rejectBad") == true { "like a careful customer: the app should log the 401 and not retry" } else { "off: record and answer as above" }}</span></div>
                            </div>

                            <div class="block row">{row_label("Lifetime")}
                                <select class="select" on:change=move |e| { let v = event_target_value(&e); if v == "remove" { confirm.set(true); } else { life.set(v) } }>
                                    {move || { let kept = saved.get()["kept"] == true; view! {
                                        {(!kept).then(|| view! { <option value="same" selected=move || life.get() == "same">{format!("Ephemeral, expires in {}", api::span(api::n(&saved.get(), "expiresAt") - api::now() as i64))}</option> })}
                                        <option value="1h" selected=move || life.get() == "1h">"Ephemeral, 1 hour from now"</option>
                                        <option value="1d" selected=move || life.get() == "1d">"Ephemeral, 1 day from now"</option>
                                        <option value="kept" selected=move || life.get() == "kept">{format!("Kept with {}", api::s(&env.get(), "env").to_uppercase())}</option>
                                        <option value="remove">"Remove inbox…"</option>
                                    } }}
                                </select></div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                        }.into_any()
                    }}
                </div>
                <div class="side">
                    <div class="caps">{move || format!("Last event to {}", name.get())}</div>
                    {move || {
                        let v = info.get();
                        let atts = v["lastEvent"]["attempts"].as_array().cloned().or_else(|| (!v["lastEvent"].is_null()).then(|| vec![v["lastEvent"].clone()])).unwrap_or_default();
                        if atts.is_empty() { return view! { <div class="small cap" style="margin-top:10px">"Nothing delivered yet."</div> }.into_any(); }
                        let mut prev = 0i64;
                        let gaps: Vec<i64> = atts.windows(2).map(|w| api::n(&w[1], "at") - api::n(&w[0], "at")).collect();
                        view! {
                            <div style="margin-top:10px">{atts.iter().map(|a| {
                                let at = api::n(a, "at");
                                let g = if prev == 0 { String::new() } else { format!("  ·  {}", gap(prev, at)) };
                                prev = at;
                                let ok = s(a, "answer").starts_with('2');
                                view! { <div class="row s12" style="padding:3px 0"><span class="mono cap" style="width:70px">{api::hms(at)}</span>
                                    <span style="width:80px">{format!("attempt {}", a["attempt"])}</span><span class:ok=ok class:danger=!ok>{format!("{}{g}", s(a, "answer"))}</span></div> }
                            }).collect_view()}</div>
                            {(!gaps.is_empty()).then(|| view! { <div class="small t2" style="margin-top:8px">{format!("The app backs off {}.", gaps.iter().map(|g| api::span(*g)).collect::<Vec<_>>().join(", "))}</div> })}
                        }.into_any()
                    }}
                    <div class="caps" style="margin-top:28px">{move || if saved.get()["kept"] == true { "webhooks/inboxes.yaml" } else { "Ephemeral · stored with the environment" }}</div>
                    <pre class="yaml" style="background:var(--surface);border:1px solid var(--rule);border-radius:6px;padding:12px">{move || s(&info.get(), "yaml")}</pre>
                    {move || (changes.get() > 0).then(|| view! { <div class="small attn">"Changed, not saved yet: the file updates on Save."</div> })}
                </div>
            </div>
            <Confirm open=confirm question=Signal::derive(move || format!("Remove {}?", name.get())) detail="Deliveries still being sent get 410 Gone. Its history is kept for 24 h." action="Remove" on_yes=remove/>
        </div>
    }
}
