//! Environments: N1 list, N2 new, N3 settings, plus the rail's switcher and the strip a paused or
//! expiring environment shows on every screen.

pub mod connections;
pub mod list;
pub mod new;
pub mod settings;

use leptos::prelude::*;
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui::{self, Pop},
};

/// "stg-a" → "STG-A"; "pr-5451" stays as typed (names with a number read better in lowercase).
pub fn label(name: &str) -> String {
    if name.chars().any(|c| c.is_ascii_digit()) { name.to_string() } else { name.to_uppercase() }
}

/// "expires in 22 h", "kept", "until the pull request closes".
pub fn lifetime(e: &Value) -> String {
    match s(e, "lifetime").as_str() {
        "expires" => {
            let left = api::n(e, "expiresAt") - api::now() as i64;
            if left <= 0 { "expiring now".into() } else { format!("expires in {}", api::span(left)) }
        }
        "pr_closed" => "until the pull request closes".into(),
        _ => "kept".into(),
    }
}

pub fn expiring_soon(e: &Value) -> bool {
    s(e, "lifetime") == "expires" && api::n(e, "expiresAt") - (api::now() as i64) < 3_600_000
}

/// "Nov 01"
pub fn day(ms: i64) -> String {
    let d = js_sys::Date::new(&(ms as f64).into());
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {:02}", M[d.get_month() as usize], d.get_date())
}

/// The environments, refreshed when any of them changes.
pub fn all() -> RwSignal<Vec<Value>> {
    let live = expect_context::<api::Live>();
    let list = RwSignal::new(Vec::<Value>::new());
    let load = move || {
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/environments").await {
                list.set(v["environments"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "environments" {
            load();
        }
        t
    });
    list
}

pub async fn act(method: &str, path: String, body: Value, done: &'static str) {
    match api::send(method, &path, body).await {
        Ok(_) => ui::toast(done),
        Err(e) => ui::toast_err(e),
    }
}

/// N1 switcher: the rail's environment box and its menu.
#[component]
pub fn Switcher() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let open = RwSignal::new(false);
    let list = RwSignal::new(Vec::<Value>::new());
    let show = move |_| {
        open.set(true);
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/environments").await {
                list.set(v["environments"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    view! {
        <div class="anchor envbox">
            <div class="env" on:click=show title="Switch environment">
                <span class="dot" class:off=move || { env.track(); !live.connected.get() || api::shared_root() } class:paused=move || env.get()["paused"] == true></span>
                <span class="grow" class:cap=move || { env.track(); api::shared_root() } class:small=move || { env.track(); api::shared_root() }>{move || if api::shared_root() { "Choose an environment".to_string() } else { label(&s(&env.get(), "env")) }}</span><span class="small cap">"▾"</span>
            </div>
            <Pop open=open>
                {move || list.get().into_iter().map(|e| {
                    let name = s(&e, "name");
                    let here = name == s(&env.get_untracked(), "env") && !api::shared_root();
                    let state = if e["paused"] == true { "paused".to_string() } else if s(&e, "lifetime") == "expires" { lifetime(&e) } else { "running".into() };
                    let n2 = name.clone();
                    view! { <div class="mi" on:click=move |_| if !here { api::switch_env(&n2, "/notifications") }>
                        <span>{label(&name)}<span class="cap">{format!("  ·  {state}")}</span></span>{here.then_some("✓")}</div> }
                }).collect_view()}
                <div class="rule"></div>
                <a class="mi" href="/environments">"All environments…"</a>
            </Pop>
        </div>
    }
}

/// A paused or about-to-expire environment says so on every screen.
#[component]
pub fn Strip() -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let name = move || s(&env.get(), "env");
    let resume = move |_| {
        let n = name();
        leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/pause"), json!({"paused": false}), "Resumed").await });
    };
    let extend = move |_| {
        let n = name();
        leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/extend"), json!({"by": "24h"}), "Extended by 24 h").await });
    };
    view! {
        {move || {
            let e = env.get();
            (e["paused"] == true).then(|| view! {
                <div class="strip">{format!("{} is paused{}: every service answers 503 and nothing is stored; state is kept.", label(&s(&e, "env")),
                    if e["pausedSince"].is_null() { String::new() } else { format!(" since {} {}", day(api::n(&e, "pausedSince")), api::hm(api::n(&e, "pausedSince"))) })}
                    <button class="link" on:click=resume>"Resume"</button></div>
            })
        }}
        {move || {
            let e = env.get();
            (e["paused"] != true && expiring_soon(&e)).then(|| view! {
                <div class="strip">{format!("{} {}. Expired environments are deleted with their state.", label(&s(&e, "env")), lifetime(&e))}
                    <button class="link" on:click=extend>"Extend 24 h"</button></div>
            })
        }}
    }
}
