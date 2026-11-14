//! HTTP APIs: each plugin is a service with tabs A1 Calls, A3 Routes, A4 Answers, A5 Auth and
//! limits; A2 Unmatched is global (top of the rail).

pub mod answers;
pub mod area;
pub mod auth;
pub mod calls;
pub mod operations;
pub mod pages;
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

/// "SERPRO Datavalid" for serpro-datavalid, from the service list.
pub fn services() -> RwSignal<Vec<Value>> {
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
    let tabs: [(&'static str, &'static str, &'static str); 6] = [("calls", "Calls", "/calls"), ("routes", "Routes", "/routes"), ("answers", "Answers", "/answers"), ("pages", "Pages", "/pages"), ("auth", "Auth & limits", "/auth"), ("settings", "Settings", "/settings")];
    // No service in this environment yet: no tabs of a service that does not exist.
    let none = service.is_empty();
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || { let c = cur.get(); let mut o = origin_line(&c); if !c["soap"].is_null() { o = format!("SOAP {}  ·  {o}", s(&c["soap"], "version")); } if o.is_empty() { "HTTP API".to_string() } else { format!("HTTP API  ·  {o}") } }}</div>
                    <h1>{move || { let c = cur.get(); if none { "No services yet".to_string() } else if c.is_null() { service.clone() } else { s(&c, "title") } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs" class:hidden=none>
                {tabs.into_iter().map(|(key, label, tail)| {
                    let nav = nav.clone();
                    let sv = cur;
                    view! {
                        <div class="tab" class:on=active == key class:hidden=move || key == "pages" && api::n(&sv.get(), "pages") == 0 on:click=move |_| nav(&format!("/apis/{}{tail}", enc(&s(&sv.get_untracked(), "name"))), Default::default())>
                            {move || if key == "routes" && !sv.get()["soap"].is_null() { "Operations" } else { label }}<span class="n">{move || match key { "calls" => api::thousands(api::n(&sv.get(), "callsToday")), "routes" if !sv.get()["soap"].is_null() => api::n(&sv.get(), "operations").to_string(), "routes" => api::n(&sv.get(), "routes").to_string(), "answers" => api::n(&sv.get(), "answers").to_string(), "pages" => api::n(&sv.get(), "pages").to_string(), _ => String::new() }}</span></div>
                    }
                }).collect_view()}
            </div>
        </div>
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
                <h1>"Unmatched"</h1><span class="sub">"calls no service took"</span><span class="grow"></span>
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
