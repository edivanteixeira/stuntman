//! WhatsApp (Meta Cloud API): W1 Conversations, W2 Templates, W3 Recipients, W4 Number settings.
//! One number per environment, so the tabs sit under the number itself (no services column).

pub mod conversations;
pub mod number;
pub mod recipients;
pub mod templates;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::Value;

use crate::api::{self, s};

/// "+1 415 555 0142" from 14155550142.
pub fn pretty(n: &str) -> String {
    let d: String = n.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.len() == 11 && d.starts_with('1') {
        format!("+1 {} {} {}", &d[1..4], &d[4..7], &d[7..])
    } else if d.len() == 12 && d.starts_with("44") {
        format!("+44 {} {} {}", &d[2..4], &d[4..8], &d[8..])
    } else if d.len() == 13 && d.starts_with("55") {
        format!("+55 {} {} {}", &d[2..4], &d[4..9], &d[9..])
    } else if d.len() > 4 {
        let (cc, rest) = d.split_at(2);
        let chunks: Vec<String> = rest.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).to_string()).collect();
        format!("+{cc} {}", chunks.join(" "))
    } else {
        n.to_string()
    }
}

pub fn status_class(st: &str) -> &'static str {
    match st {
        "failed" => "danger",
        "received" | "replied" => "ok",
        "accepted" | "sent" => "cap",
        _ => "",
    }
}

pub fn error_name(code: i64) -> &'static str {
    match code {
        131026 => "Recipient not on WhatsApp",
        131047 => "More than 24 h since the last reply",
        131049 => "Not delivered (ecosystem health)",
        131051 => "Unsupported message type",
        130429 => "Rate limit hit",
        131048 => "Spam rate limit hit",
        131000 => "Something went wrong",
        _ => "",
    }
}

/// Number block, refreshed on live changes.
pub fn number() -> RwSignal<Value> {
    let live = expect_context::<api::Live>();
    let n = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/whatsapp/number").await { n.set(v); } });
    });
    n
}

#[component]
pub fn Head(active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let env = expect_context::<RwSignal<Value>>();
    let live = expect_context::<api::Live>();
    let num = number();
    let counts = RwSignal::new((0i64, 0usize, 0usize));
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move {
            let c = api::get("/whatsapp/conversations").await.ok().and_then(|v| v["conversations"].as_array().map(|a| a.len() as i64)).unwrap_or(0);
            let t = api::get("/whatsapp/templates").await.ok().and_then(|v| v["templates"].as_array().map(|a| a.len())).unwrap_or(0);
            let r = api::get("/whatsapp/recipients").await.ok().and_then(|v| v["recipients"].as_array().map(|a| a.len())).unwrap_or(0);
            counts.set((c, t, r));
        });
    });
    let tabs: [(&'static str, &'static str, &'static str); 4] = [("conversations", "Conversations", "/whatsapp"), ("templates", "Templates", "/whatsapp/templates"), ("recipients", "Recipients", "/whatsapp/recipients"), ("number", "Number settings", "/whatsapp/number")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("WhatsApp  ·  Meta Cloud API  ·  {}", api::s(&env.get(), "env").to_uppercase())}</div>
                    <h1>{move || { let n = &num.get()["number"]; let name = s(&n["display"], "name"); if name.is_empty() { "WhatsApp".to_string() } else { format!("{name}  ·  {}", pretty(&s(&n["display"], "number"))) } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label, path)| {
                    let nav = nav.clone();
                    view! { <div class="tab" class:on=active == key on:click=move |_| nav(path, Default::default())>{label}
                        <span class="n">{move || match key { "conversations" => counts.get().0.to_string(), "templates" => counts.get().1.to_string(), "recipients" => counts.get().2.to_string(), _ => String::new() }}</span></div> }
                }).collect_view()}
            </div>
        </div>
    }
}

/// The YAML with the lines that changed since `before` marked (left bar), as the frames show.
#[component]
pub fn YamlMarked(#[prop(into)] text: Signal<String>, #[prop(into)] before: Signal<String>) -> impl IntoView {
    view! {
        <pre class="yaml envyaml">{move || {
            let b = before.get();
            let old: std::collections::HashSet<&str> = b.lines().collect();
            let fresh = !b.is_empty();
            text.get().lines().map(|l| { let changed = fresh && !old.contains(l); view! { <div class:yl-changed=changed>{l.to_string()}{"\u{200b}"}</div> } }).collect_view()
        }}</pre>
        {move || { let (a, b) = (text.get(), before.get()); (!b.is_empty() && a != b).then(|| view! { <div class="small cap">"The changed lines are marked."</div> }) }}
    }
}
