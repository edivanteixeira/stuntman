//! Webhook inboxes: H1 Deliveries and H2 Behaviour. Shares the inbox list and the header.

pub mod behaviour;
pub mod deliveries;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui::{self, Dialog},
};

/// Inboxes + Live: the list every webhook screen shows on the left.
#[derive(Clone, Copy)]
pub struct Shared {
    pub inboxes: RwSignal<Vec<Value>>,
    pub total: RwSignal<i64>,
}

pub fn shared() -> Shared {
    let live = expect_context::<api::Live>();
    let sh = Shared { inboxes: RwSignal::new(vec![]), total: RwSignal::new(0) };
    let load = move || {
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/webhooks/inboxes").await {
                sh.inboxes.set(v["inboxes"].as_array().cloned().unwrap_or_default());
            }
            if let Ok(v) = api::get("/webhooks/deliveries?limit=1&since=1h").await {
                sh.total.set(api::n(&v, "total"));
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && topic.starts_with("webhook.") {
            load();
        }
        t
    });
    sh
}

#[component]
pub fn Head(active: &'static str, #[prop(into)] name: Signal<String>, #[prop(into)] right: ViewFn, sh: Shared) -> impl IntoView {
    let nav = use_navigate();
    let (n2, n3) = (nav.clone(), nav);
    view! {
        <div class="head">
            <div class="top">
                <h1>"Inboxes"</h1><span class="sub">"webhooks your app sends to its customers"</span>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                <div class="tab" class:on=active == "deliveries" on:click=move |_| { let n = name.get_untracked(); n2(&if n.is_empty() { "/inboxes/webhooks".into() } else { format!("/inboxes/webhooks/{n}") }, Default::default()) }>
                    "Deliveries"<span class="n">{move || sh.total.get()}</span></div>
                <div class="tab" class:on=active == "behaviour" on:click=move |_| { let n = name.get_untracked(); if !n.is_empty() { n3(&format!("/inboxes/webhooks/{n}/behaviour"), Default::default()) } }>"Behaviour"</div>
            </div>
        </div>
    }
}

#[component]
pub fn InboxList(sh: Shared, #[prop(into)] selected: Signal<String>, tab: &'static str) -> impl IntoView {
    let nav = use_navigate();
    let creating = RwSignal::new(false);
    let clock = RwSignal::new(0u32);
    set_interval(move || clock.update(|c| *c += 1), std::time::Duration::from_secs(20));
    let row = move |v: Value| {
        let i = v["inbox"].clone();
        let n = s(&i, "name");
        let (n2, n3) = (n.clone(), n.clone());
        let nav = nav.clone();
        let sub = if v["kind"] == "kept" {
            s(&v, "summary")
        } else if v["expired"] == true {
            "expired · later deliveries get 410".to_string()
        } else {
            let left = api::n(&i, "expiresAt") - api::now() as i64;
            let by = s(&i, "createdBy");
            let by = by.trim_start_matches("mcp:").trim_start_matches("api:");
            format!("expires in {}{}", api::span(left), if by.is_empty() { String::new() } else { format!(" · {by}") })
        };
        let eph = v["kind"] != "kept";
        view! {
            <div class="srow" class:on=move || selected.get() == n2 style="padding:10px 24px"
                on:click=move |_| nav(&if tab == "behaviour" { format!("/inboxes/webhooks/{n3}/behaviour") } else { format!("/inboxes/webhooks/{n3}") }, Default::default())>
                <div class="n">{n}</div><div class="d" class:attn=eph>{sub}</div>
            </div>
        }
    };
    let row = StoredValue::new(row);
    view! {
        <div class="list narrow">
            {move || {
                let _ = clock.get();
                let all = sh.inboxes.get();
                let kept: Vec<Value> = all.iter().filter(|v| v["kind"] == "kept").cloned().collect();
                let eph: Vec<Value> = all.iter().filter(|v| v["kind"] != "kept").cloned().collect();
                view! {
                    <div class="section caps" style="padding-top:16px">"Kept"</div>
                    {if kept.is_empty() { view! { <div class="note" style="padding-top:4px">"None yet."</div> }.into_any() } else { kept.into_iter().map(|v| row.with_value(|r| r(v))).collect_view().into_any() }}
                    <div class="section caps">"Ephemeral"</div>
                    {if eph.is_empty() { view! { <div class="note" style="padding-top:4px">"Tests and agents make them with create_webhook_inbox()."</div> }.into_any() } else { eph.into_iter().map(|v| row.with_value(|r| r(v))).collect_view().into_any() }}
                }
            }}
            <div class="srow" style="padding:12px 24px"><button class="link" style="font-size:13px" on:click=move |_| creating.set(true)>"+ New inbox"</button></div>
            <div class="note">"Kept inboxes live with the environment; ephemeral ones are made by a test or an agent and delete themselves."</div>
            <NewInbox open=creating/>
        </div>
    }
}

#[component]
fn NewInbox(open: RwSignal<bool>) -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let name = RwSignal::new(String::new());
    let life = RwSignal::new("1h".to_string());
    let err = RwSignal::new(String::new());
    Effect::new(move |_| {
        if open.get() {
            name.set(String::new());
            err.set(String::new());
        }
    });
    let create = move |_| {
        let n = name.get_untracked().trim().to_lowercase();
        let l = life.get_untracked();
        let body = if l == "kept" { json!({"name": n, "keep": true}) } else { json!({"name": if n.is_empty() { Value::Null } else { json!(n) }, "expires": l}) };
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", "/webhooks/inboxes", body).await {
                Ok(v) => {
                    let u = s(&v, "url");
                    if let Some(w) = web_sys::window() {
                        let _ = w.navigator().clipboard().write_text(&u);
                    }
                    ui::toast(format!("{u} created and copied"));
                    open.set(false);
                    nav(&format!("/inboxes/webhooks/{}/behaviour", s(&v, "name")), Default::default());
                }
                Err(e) => err.set(e),
            }
        });
    };
    view! {
        <Dialog open=open title="New inbox">
            <div class="field"><label>"Name"</label>
                <input class="input mono" placeholder=move || if life.get() == "kept" { "globex" } else { "random when left empty" } prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
                <span class="small cap">"Lowercase letters, digits and dashes; it is part of the URL."</span></div>
            <div style="margin-top:14px">
                {[("1h", "Ephemeral, 1 hour".to_string()), ("1d", "Ephemeral, 1 day".to_string())].into_iter().map(|(v, l)| view! {
                    <div class="radio" class:on=move || life.get() == v on:click=move |_| life.set(v.to_string())><span class="r"></span>{l}</div> }).collect_view()}
                <div class="radio" class:on=move || life.get() == "kept" on:click=move |_| life.set("kept".into())><span class="r"></span>{move || format!("Kept with {}", api::s(&env.get(), "env").to_uppercase())}</div>
            </div>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
            <div class="actions"><button class="btn" on:click=move |_| open.set(false)>"Cancel"</button><button class="btn primary" on:click={ let c = create.clone(); move |e| c(e) }>"Create inbox"</button></div>
        </Dialog>
    }
}

/// "+1 min", "+3 min": the gaps between attempts of one event.
pub fn gap(prev: i64, at: i64) -> String {
    let d = (at - prev).max(0);
    if d < 60_000 { format!("{:.0} s later", d as f64 / 1000.0) } else { format!("{} later", api::span(d)) }
}
