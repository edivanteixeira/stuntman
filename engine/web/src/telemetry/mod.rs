//! Telemetry: T1 Traces, T2 Logs, T3 Exporter settings, T4 Metrics.

pub mod logs;
pub mod metrics;
pub mod settings;
pub mod traces;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui::{self, Pop},
};

/// Shared state of the three tabs: time range, pause, overview counts.
#[derive(Clone, Copy)]
pub struct Shared {
    pub since: RwSignal<String>,
    pub paused: RwSignal<bool>,
    pub paused_at: RwSignal<i64>,
    pub overview: RwSignal<Value>,
    /// Bumps on live telemetry changes while not paused.
    pub tick: RwSignal<u64>,
    pub new_while_paused: RwSignal<u32>,
}

pub fn shared() -> Shared {
    let live = expect_context::<api::Live>();
    let sh = Shared {
        since: RwSignal::new("15m".into()),
        paused: RwSignal::new(false),
        paused_at: RwSignal::new(0),
        overview: RwSignal::new(Value::Null),
        tick: RwSignal::new(0),
        new_while_paused: RwSignal::new(0),
    };
    let load = move || {
        let s = sh.since.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/telemetry/overview?since={s}")).await {
                sh.overview.set(v);
            }
        });
    };
    load();
    Effect::new(move |_| {
        sh.since.track();
        load();
        sh.tick.update(|t| *t += 1);
    });
    // Live: many batches per second can arrive; refresh at most every 2 s.
    let pending = RwSignal::new(false);
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && topic.starts_with("telemetry.") {
            if sh.paused.get_untracked() {
                if topic == "telemetry.traces" {
                    sh.new_while_paused.update(|n| *n += 1);
                }
            } else if !pending.get_untracked() {
                pending.set(true);
                set_timeout(move || {
                    pending.set(false);
                    load();
                    sh.tick.update(|t| *t += 1);
                }, std::time::Duration::from_millis(2000));
            }
        }
        t
    });
    sh
}

pub fn since_label(s: &str) -> &'static str {
    match s {
        "1h" => "Last hour",
        "24h" => "Last 24 h",
        "5m" => "Last 5 min",
        _ => "Last 15 min",
    }
}

#[component]
pub fn Head(active: &'static str, sh: Shared, q: RwSignal<String>, placeholder: &'static str) -> impl IntoView {
    let nav = use_navigate();
    let range = RwSignal::new(false);
    let (n1, n2, n3) = (nav.clone(), nav.clone(), nav);
    let ov = sh.overview;
    view! {
        <div class="head">
            <div class="top">
                <h1>"Telemetry"</h1><span class="grow"></span>
                <input class="input" style="width:200px" placeholder=placeholder prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                <div class="anchor">
                    <button class="btn" on:click=move |_| range.set(true)>{move || since_label(&sh.since.get())}<span class="small">"▾"</span></button>
                    <Pop open=range right=true>
                        <div class="mh caps">"Live"</div>
                        {[("5m", "Last 5 min"), ("15m", "Last 15 min"), ("1h", "Last hour"), ("24h", "Last 24 h")].into_iter().map(|(v, l)| view! {
                            <div class="mi" on:click=move |_| sh.since.set(v.into())><span>{l}</span>{move || (sh.since.get() == v).then_some("✓")}</div> }).collect_view()}
                    </Pop>
                </div>
                {move || if sh.paused.get() {
                    view! { <button class="btn" on:click=move |_| { sh.paused.set(false); sh.new_while_paused.set(0); sh.tick.update(|t| *t += 1); }>"Resume"</button> }.into_any()
                } else {
                    view! { <button class="btn" on:click=move |_| { sh.paused.set(true); sh.paused_at.set(api::now() as i64); }>"Pause"</button> }.into_any()
                }}
            </div>
            <div class="tabs">
                <div class="tab" class:on=active == "traces" on:click=move |_| n1("/telemetry/traces", Default::default())>"Traces"<span class="n">{move || api::n(&ov.get(), "traces")}</span></div>
                <div class="tab" class:on=active == "logs" on:click=move |_| n2("/telemetry/logs", Default::default())>"Logs"<span class="n">{move || api::n(&ov.get(), "logs")}</span></div>
                <div class="tab" class:on=active == "metrics" on:click=move |_| n3("/telemetry/metrics", Default::default())>"Metrics"<span class="n">{move || api::n(&ov.get(), "series")}</span></div>
            </div>
        </div>
        <Strips sh=sh/>
    }
}

/// T1/T2 states: paused, the collector provoked on purpose, a signal turned off.
#[component]
fn Strips(sh: Shared) -> impl IntoView {
    let stop = move |_| {
        leptos::task::spawn_local(async move {
            match api::send("DELETE", "/telemetry/collector/provoke", json!({})).await {
                Ok(_) => sh.tick.update(|t| *t += 1),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        {move || sh.paused.get().then(|| view! {
            <div class="strip">{format!("Paused at {}  ·  {} new batches arrived", api::hms(sh.paused_at.get()), sh.new_while_paused.get())}
                <button class="link" on:click=move |_| { sh.paused.set(false); sh.new_while_paused.set(0); sh.tick.update(|t| *t += 1); }>"Resume"</button></div>
        })}
        {move || {
            let st = sh.overview.get()["settings"].clone();
            let c = st["collector"].clone();
            let on = s(&c, "answer") != "normal" && !s(&c, "answer").is_empty() && c["until"].as_i64().map(|u| u > api::now() as i64).unwrap_or(true);
            on.then(|| {
                let what = match s(&c, "answer").as_str() { "refused" => "refuses connections".to_string(), "slow" => "answers slowly (5 s per batch)".into(), "drop_half" => "drops half of the batches".into(), a => format!("answers {a} to every batch") };
                view! { <div class="strip dark">{format!("The collector {what} on purpose  ·  since {}  ·  {}", api::hm(api::n(&c, "since")), if s(&c, "onlyFor") == "all" { "all services".to_string() } else { s(&c, "onlyFor") })}
                    <span class="small" style="opacity:.8">{format!("{} batches refused so far. If the app also stops calling its providers, it is blocking on telemetry.", api::n(&sh.overview.get(), "refused"))}</span>
                    <span class="grow"></span><button class="btn small" on:click=stop>"Stop"</button></div> }
            })
        }}
        {move || {
            let ov = sh.overview.get();
            let off: Vec<String> = ["traces", "logs", "metrics"].iter().filter(|k| ov["settings"]["signals"][**k]["on"] == false).map(|k| k.to_string()).collect();
            (!off.is_empty()).then(|| view! {
                <div class="strip">{format!("{} turned off for {}: Stuntman answers 200 and drops them, so the app never retries or blocks. Dropped in the last 15 min: {}.",
                    { let j = off.join(", "); let mut c = j.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }, s(&ov["settings"], "environment").to_uppercase(),
                    off.iter().map(|k| format!("{} {}", api::thousands(api::n(&ov["dropped"], k)), match k.as_str() { "traces" => "spans", "logs" => "log lines", _ => "data points" })).collect::<Vec<_>>().join(", "))}
                    <a class="link" href="/telemetry/settings">"Exporter settings ›"</a></div>
            })
        }}
    }
}

/// The receiving block and the no-data state share these.
pub fn env_lines(ov: &Value) -> String {
    s(&ov["endpoints"], "env")
}

/// Colour of a provider line: the trip's service, or the host's kind.
pub fn line_colour(span: &Value) -> &'static str {
    let module = s(&span["trip"], "module");
    let host = format!("{} {}", s(span, "host"), s(span, "name")).to_lowercase();
    match module.as_str() {
        "email" => "var(--line-email)",
        "webhook" => "#00A3AD",
        _ if host.contains("mail") || host.contains("sendgrid") => "var(--line-email)",
        _ if host.contains("sb.") || host.contains("queue") || host.contains("service bus") => "#E8620C",
        _ => "var(--line-blue)",
    }
}
