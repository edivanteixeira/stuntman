//! N1 · Environments: every environment on this Stuntman as a card; the detail of the selected one
//! with its addresses, what is in it and the actions (open, clone, pause, reset).

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{act, all, day, expiring_soon, label, lifetime};
use crate::{
    api::{self, s},
    ui::Confirm,
};

fn line(e: &Value) -> String {
    let c = &e["counts"];
    if e["paused"] == true {
        let since = if e["pausedSince"].is_null() { String::new() } else { format!(" since {}", day(api::n(e, "pausedSince"))) };
        return format!("state kept{since} · answers 503 to everything");
    }
    let calls = api::n(c, "callsLastHour");
    let traffic = if calls > 0 {
        format!("{} calls in the last hour", api::thousands(calls))
    } else if c["lastCallAt"].is_null() {
        "no calls yet".into()
    } else {
        format!("quiet for {}", api::span(api::now() as i64 - api::n(c, "lastCallAt")))
    };
    let from = s(e, "clonedFrom");
    if from.is_empty() {
        format!("{} services · {traffic}", api::n(c, "services"))
    } else {
        let by = s(e, "createdBy").replace("api:", "").replace("mcp:", "");
        format!("cloned from {}{} · {traffic}", label(&from), if by.is_empty() || by == "you" { String::new() } else { format!(" by {by}") })
    }
}

#[component]
pub fn List() -> impl IntoView {
    let nav = use_navigate();
    let q = use_query_map();
    let env = expect_context::<RwSignal<Value>>();
    let list = all();
    let sel = RwSignal::new(String::new());
    Effect::new(move |_| {
        if let Some(n) = q.get().get("sel") {
            sel.set(n);
        }
    });
    let current = Memo::new(move |_| {
        let l = list.get();
        let want = if sel.get().is_empty() { s(&env.get(), "env") } else { sel.get() };
        l.iter().find(|e| s(e, "name") == want).or(l.first()).cloned().unwrap_or(Value::Null)
    });
    let reset = RwSignal::new(false);
    let n_new = nav.clone();
    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <h1>"Environments"</h1>
                    <span class="sub">{move || format!("{} on this Stuntman · each with its own services, state and URLs", list.get().len())}</span>
                    <span class="grow"></span>
                    <button class="btn primary" on:click=move |_| n_new("/environments/new", Default::default())>"New environment"</button>
                </div>
            </div>
            <div class="body">
                <div class="pane pad">
                    {move || list.get().into_iter().map(|e| {
                        let name = s(&e, "name");
                        let n2 = name.clone();
                        let on = move || s(&current.get(), "name") == n2;
                        let paused = e["paused"] == true;
                        let soon = expiring_soon(&e) || s(&e, "lifetime") == "expires";
                        view! {
                            <div class="envcard" class:on=on on:click=move |_| sel.set(name.clone())>
                                <div class="row">
                                    <span class="dot" class:paused=paused></span>
                                    <span class="nm">{label(&s(&e, "name"))}</span>
                                    <span class="state" class:attn=paused class:ok=!paused>{if paused { "paused" } else { "running" }}</span>
                                    <span class="grow"></span>
                                    <span class="small" class:attn=soon class:cap=!soon>{lifetime(&e)}</span>
                                </div>
                                <div class="l2 t2">{line(&e)}</div>
                                <div class="mono small cap">{s(&e, "baseDomain")}</div>
                            </div>
                        }
                    }).collect_view()}
                    {move || (list.get().len() == 1).then(|| {
                        let nav = nav.clone();
                        view! {
                            <div class="callout">
                                <div class="t">{format!("Just {}", label(&s(&list.get()[0], "name")))}</div>
                                <div class="t2">"Most local setups need one. Make more for each staging copy, or let the pipeline make one per pull request."</div>
                                <button class="link" on:click=move |_| nav("/environments/new", Default::default())>"New environment"</button>
                            </div>
                        }
                    })}
                    <div class="small cap" style="margin-top:16px">"Environments are folders under environments/; each one can be paused, cloned or set to expire."</div>
                </div>
                <div class="side envside">
                    {move || {
                        let e = current.get();
                        if e.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let name = s(&e, "name");
                        let paused = e["paused"] == true;
                        let a = e["addresses"].clone();
                        let c = e["counts"].clone();
                        let clients: Vec<String> = e["clients"].as_array().cloned().unwrap_or_default().iter().map(|x| s(x, "name")).collect();
                        let (n_open, n_clone, n_pause, n_ext) = (name.clone(), name.clone(), name.clone(), name.clone());
                        let here = name == s(&env.get_untracked(), "env");
                        let since = if paused { api::n(&e, "pausedSince") } else { api::n(&e, "createdAt") };
                        view! {
                            <h2 class="h2">{label(&name)}</h2>
                            <div class="small" class:ok=!paused class:attn=paused>{format!("{} since {}  ·  {}", if paused { "paused" } else { "running" }, day(since), lifetime(&e))}</div>
                            {paused.then(|| { let n = name.clone(); view! {
                                <div class="callout attn-l"><div class="t attn">{format!("{} is paused", label(&n))}</div>
                                    <div class="t2">{format!("Every service answers 503 and nothing is stored; state from {} is kept. Resume brings it back as it was.", day(api::n(&e, "pausedSince")))}</div></div> } })}
                            {(!paused && expiring_soon(&e)).then(|| { let n = n_ext.clone(); view! {
                                <div class="callout attn-l"><div class="t attn">{format!("{} {}", label(&n), lifetime(&e))}</div>
                                    <div class="t2">"Extend it, or keep it. Expired environments are deleted with their state."</div>
                                    <button class="link" on:click=move |_| { let n = n.clone(); leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/extend"), json!({"by": "24h"}), "Extended by 24 h").await }) }>"Extend 24 h"</button></div> } })}
                            <div class="caps" style="margin:22px 0 10px">"Addresses"</div>
                            <table class="kv2">
                                <tr><td>"Services"</td><td class="mono">{s(&a, "services")}</td></tr>
                                <tr><td>"Callbacks go to"</td><td class="mono">{if s(&a, "callbacks").is_empty() { "each service's own URL".to_string() } else { s(&a, "callbacks") }}</td></tr>
                                <tr><td>"MCP"</td><td class="mono">{s(&a, "mcp")}</td></tr>
                                <tr><td>"Control API"</td><td class="mono">{s(&a, "api")}</td></tr>
                                <tr><td>"Without hosts entries"</td><td class="mono">{s(&a, "local")}</td></tr>
                            </table>
                            <div class="caps" style="margin:22px 0 10px">"In it"</div>
                            <table class="kv2">
                                <tr><td>"Services"</td><td>{api::n(&c, "services")}</td></tr>
                                <tr><td>"Inboxes"</td><td>{api::n(&c, "inboxes")}</td></tr>
                                <tr><td>"Calls in the last hour"</td><td>{api::thousands(api::n(&c, "callsLastHour"))}</td></tr>
                                <tr><td>"Connected now"</td><td>{if clients.is_empty() { "nobody".to_string() } else { format!("{} ({})", clients.len(), clients.join(", ")) }}</td></tr>
                                <tr><td>"State on disk"</td><td>{api::kb(api::n(&e["sizes"], "state"))}</td></tr>
                            </table>
                            <div class="row wrap" style="margin-top:18px;gap:8px">
                                <button class="btn" disabled=here on:click=move |_| api::switch_env(&n_open, "/notifications")>"Open"</button>
                                <a class="btn" href=format!("/environments/new?from={n_clone}")>"Clone"</a>
                                <button class="btn" on:click=move |_| { let n = n_pause.clone(); leptos::task::spawn_local(async move {
                                    act("POST", format!("/environments/{n}/pause"), json!({"paused": !paused}), if paused { "Resumed" } else { "Paused: every service answers 503" }).await }) }>
                                    {if paused { "Resume" } else { "Pause" }}</button>
                                <button class="btn" on:click=move |_| reset.set(true)>"Reset to seeds"</button>
                            </div>
                            <a class="link" style="display:inline-block;margin-top:18px;font-size:13px" href=format!("/environments/{name}")>"Environment settings ›"</a>
                        }.into_any()
                    }}
                </div>
            </div>
            <Confirm open=reset question=Signal::derive(move || format!("Reset {} to its seeds?", label(&s(&current.get(), "name"))))
                detail=Signal::derive(|| "Messages, webhook deliveries, telemetry and temporary inboxes are deleted. Service files, kept inboxes and secrets stay. This cannot be undone.".to_string())
                action="Reset to seeds" on_yes=move |_| { let n = s(&current.get_untracked(), "name"); leptos::task::spawn_local(async move { act("POST", format!("/environments/{n}/reset"), json!({}), "Reset to seeds").await }) }/>
        </div>
    }
}
