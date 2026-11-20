//! HTTP APIs: each plugin is a service with tabs A1 Calls, A3 Routes, A4 Answers, A5 Auth and
//! limits; A2 Unmatched is global (top of the rail).

pub mod answers;
pub mod area;
pub mod auth;
pub mod calls;
pub mod mcp;
pub mod operations;
pub mod pages;
pub mod review;
pub mod routes;
pub mod settings;
pub mod unmatched;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::Value;

use crate::{
    api::{self, enc, s},
    ui::Pop,
};

#[derive(Clone, Copy)]
pub struct Services(pub RwSignal<Vec<Value>>);

/// The Calls range, kept by the area while you move between tabs and services.
#[derive(Clone, Copy)]
pub struct Range(pub RwSignal<String>);

/// "SERPRO Datavalid" for serpro-datavalid, from the service list.
pub fn services() -> RwSignal<Vec<Value>> {
    // Inside the HTTP APIs area the rail's list is shared: one fetch per change, not one per panel.
    if let Some(Services(l)) = use_context::<Services>() {
        return l;
    }
    let live = expect_context::<api::Live>();
    let list = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/apis/services").await {
                list.set(v["services"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    list
}

/// Every HTTP API service opens with this header: kind on top, its name, then the tabs
/// Calls · Routes · Answers · Auth & limits · Settings (the same slots every service kind has).
#[component]
pub fn PluginHead(service: String, active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let list = services();
    let sv = service.clone();
    let cur = Memo::new(move |_| list.get().into_iter().find(|x| s(x, "name") == sv).unwrap_or(Value::Null));
    let tabs: [(&'static str, &'static str, &'static str); 8] = [("review", "Review", "/review"), ("calls", "Calls", "/calls"), ("routes", "Routes", "/routes"), ("tools", "Tools", "/tools"), ("answers", "Answers", "/answers"), ("pages", "Pages", "/pages"), ("auth", "Auth & limits", "/auth"), ("settings", "Settings", "/settings")];
    // No service in this environment yet: no tabs of a service that does not exist.
    let none = service.is_empty();
    // An old link, or another environment: the list came and this service is not in it.
    let missing = Memo::new(move |_| !none && !list.get().is_empty() && cur.get().is_null());
    let svc = service.clone();
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || { let c = cur.get(); let mut o = origin_line(&c); if !c["soap"].is_null() { o = format!("SOAP {}  ·  {o}", s(&c["soap"], "version")); } else if !c["spec"].is_null() { o = format!("{} {}  ·  {o}", if s(&c["spec"], "version").starts_with('2') { "Swagger" } else { "OpenAPI" }, s(&c["spec"], "version")); } else if !c["record"].is_null() { o = format!("recording  ·  {}", s(&c["record"], "upstream").split("://").nth(1).unwrap_or("")); } if o.is_empty() { "HTTP API".to_string() } else { format!("HTTP API  ·  {o}") } }}</div>
                    <h1>{move || { let c = cur.get(); if none { "No services yet".to_string() } else if missing.get() { format!("No service “{service}” in this environment") } else if c.is_null() { service.clone() } else { s(&c, "title") } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs" class:hidden=move || none || missing.get()>
                {tabs.into_iter().map(|(key, label, tail)| {
                    let nav = nav.clone();
                    let sv = cur;
                    // The service in the URL, not the list: the list may still be loading.
                    let to = format!("/apis/{}{tail}", enc(&svc));
                    view! {
                        <div class="tab" class:on=active == key class:hidden=move || (key == "pages" && api::n(&sv.get(), "pages") == 0) || (key == "review" && sv.get()["record"].is_null()) || (key == "tools" && sv.get()["mcp"].is_null()) on:click=move |_| nav(&to, Default::default())>
                            {move || if key == "routes" && !sv.get()["soap"].is_null() { "Operations" } else { label }}<span class="n">{move || match key { "calls" => api::thousands(api::n(&sv.get(), "callsToday")), "routes" if !sv.get()["soap"].is_null() => api::n(&sv.get(), "operations").to_string(), "routes" => api::n(&sv.get(), "routes").to_string(), "answers" => api::n(&sv.get(), "answers").to_string(), "pages" => api::n(&sv.get(), "pages").to_string(), "review" => api::n(&sv.get(), "recorded").to_string(), _ => String::new() }}</span></div>
                    }
                }).collect_view()}
            </div>
        </div>
        <RecordingStrip cur=cur/>
    }
}

/// Time range, search and Live, on the right of Calls and Unmatched.
#[component]
pub fn RangeSearch(since: RwSignal<String>, q: RwSignal<String>) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let range = RwSignal::new(false);
    view! {
        <span class="anchor"><button class="btn" on:click=move |_| range.set(true)>{move || match since.get().as_str() { "1h" => "Last hour", "1d" => "Last 24 h", "5m" => "Last 5 min", _ => "Last 15 min" }}<span class="small">" ▾"</span></button>
            <Pop open=range right=true>
                {[("5m", "Last 5 min"), ("15m", "Last 15 min"), ("1h", "Last hour"), ("1d", "Last 24 h")].into_iter().map(|(v, l)| view! { <div class="mi" on:click=move |_| since.set(v.into())><span>{l}</span>{move || (since.get() == v).then_some("✓")}</div> }).collect_view()}
            </Pop></span>
        <input class="input" style="width:150px" placeholder="Path, id, CPF…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
        <span class="row small" style="gap:6px"><span class="dot" class:off=move || !live.connected.get()></span>"Live"</span>
    }
}

/// Unmatched (global, top of the rail): calls no service took.
#[component]
pub fn UnmatchedHead(since: RwSignal<String>, q: RwSignal<String>, unmatched: Signal<i64>) -> impl IntoView {
    view! {
        <div class="head">
            <div class="top">
                <h1>"Not answered"</h1><span class="sub">"calls no service took"</span><span class="grow"></span>
                <RangeSearch since=since q=q/>
            </div>
            <div class="tabs">
                <div class="tab on">"HTTP APIs"<span class="n">{move || unmatched.get()}</span></div>
            </div>
        </div>
    }
}

pub fn origin_line(x: &Value) -> String {
    let mut v = vec![];
    if x["billable"] == true {
        v.push("billable".to_string());
    }
    v.push(s(x, "origin"));
    if !s(x, "region").is_empty() {
        v.push(s(x, "region"));
    }
    v.join("  ·  ")
}

/// The answer name's colour on lists: errors danger, provoked/odd attention, normal success.
pub fn answer_class(status: i64, answer: &str) -> &'static str {
    if status == 0 || status >= 500 || ["reset", "empty", "timeout"].contains(&answer) {
        "danger"
    } else if status >= 400 {
        "danger"
    } else if ["approved", "human", "passed", "created", "done", "registered", "token", "ok"].contains(&answer) {
        "ok"
    } else {
        "attn"
    }
}

/// A9: while a service records the real provider, every tab says so, with Pause and Stop.
#[component]
fn RecordingStrip(cur: Memo<Value>) -> impl IntoView {
    let nav = use_navigate();
    let act = move |method: &'static str, tail: &'static str, body: serde_json::Value, then: Option<&'static str>| {
        let svc = s(&cur.get_untracked(), "name");
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send(method, &format!("/plugins/{}/recording{tail}", enc(&svc)), body).await {
                Ok(_) => if let Some(t) = then { nav(&format!("/apis/{}{t}", enc(&svc)), Default::default()); },
                Err(e) => crate::ui::toast_err(e),
            }
        });
    };
    let (a1, a2) = (act.clone(), act);
    move || {
        let c = cur.get();
        let r = c["record"].clone();
        if r.is_null() {
            return None;
        }
        let host = s(&r, "upstream").split("://").nth(1).unwrap_or("").trim_end_matches('/').to_string();
        let n = api::n(&c, "recorded");
        let (a1, a2) = (a1.clone(), a2.clone());
        Some(if r["stopped"].as_i64().unwrap_or(0) > 0 || r["live"] != true && r["paused"] != true {
            view! { <div class="strip"><span class="grow">{format!("Recording stopped: {n} calls to {host}. Review them and make the plugin.")}</span>
                <a class="link" href=format!("/apis/{}/review", enc(&s(&c, "name")))>"Review ›"</a></div> }.into_any()
        } else {
            let paused = r["paused"] == true;
            let left = (r["until"].as_i64().unwrap_or(0) - js_sys::Date::now() as i64).max(0) / 60_000;
            view! { <div class="strip rec"><span class="dot rec"></span><span class="grow">{if paused { format!("Recording paused  ·  {n} calls kept") } else { format!("Recording  ·  {} goes to {host}  ·  {n} kept  ·  {left} min left", if r["mode"] == "unknown" { "every unknown call" } else { "every call" }) }}</span>
                <button class="link" on:click=move |_| a1("POST", "/pause", serde_json::json!({"resume": paused}), None)>{if paused { "Resume" } else { "Pause" }}</button>
                <button class="btn small primary" on:click=move |_| a2("DELETE", "", serde_json::json!({}), Some("/review"))>"Stop and review"</button></div> }.into_any()
        })
    }
}
