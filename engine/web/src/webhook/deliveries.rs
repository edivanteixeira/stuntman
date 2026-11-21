//! H1 · Inboxes › Deliveries: what the app sent to an inbox, with signature, replay, timing and
//! trap checks, the answer it got, and every attempt of the same event.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use super::{gap, Head, InboxList};
use crate::{
    api::{self, s},
    ui::{self, Pop},
};

fn sig_line(d: &Value) -> (String, &'static str) {
    let sg = &d["signature"];
    match s(sg, "result").as_str() {
        "valid" if !s(sg, "warning").is_empty() => ("signed ✓ · timestamp not signed".into(), "attn"),
        "valid" => ("signed ✓".into(), "ok"),
        "old_secret" => ("signature ✕ (old secret)".into(), "danger"),
        "stale" => ("signed ✓ · timestamp too old".into(), "danger"),
        "invalid" => ("signature ✕".into(), "danger"),
        "missing" => ("no signature".into(), "danger"),
        _ => ("not checked".into(), "cap"),
    }
}

fn notes(d: &Value) -> Vec<String> {
    let mut v = vec![];
    if let Some(dup) = d["duplicateOf"].as_object() {
        v.push(format!("duplicate of {}", api::hms(dup.get("at").and_then(|x| x.as_i64()).unwrap_or(0))));
    }
    let age = d["signature"]["timestampAgeMs"].as_i64().unwrap_or(0);
    if age > 120_000 {
        v.push(format!("timestamp {} old", api::span(age)));
    }
    if d["attempt"].as_i64().unwrap_or(1) > 1 && d["duplicateOf"].is_null() {
        v.push(format!("attempt {}", d["attempt"]));
    }
    if let Some(g) = d["gaveUpMs"].as_i64() {
        v.push(format!("client gave up at {:.1} s", g as f64 / 1000.0));
    }
    for t in d["traps"].as_array().cloned().unwrap_or_default() {
        v.push(format!("trap {}: {}", s(&t, "trap").replace('_', " "), s(&t, "detail")));
    }
    v
}

#[component]
pub fn Deliveries() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let sh = super::shared();
    let name = Memo::new(move |_| params.get().get("name").unwrap_or_default());
    let since = RwSignal::new("1h".to_string());
    let since_open = RwSignal::new(false);
    let list = RwSignal::new(Vec::<Value>::new());
    let total = RwSignal::new(0i64);
    let selected = RwSignal::new(String::new());
    let detail = RwSignal::new(Value::Null);

    // Pick the first inbox when none is in the URL.
    let nav2 = nav.clone();
    Effect::new(move |_| {
        if name.get().is_empty() {
            if let Some(f) = sh.inboxes.get().first() {
                nav2(&format!("/inboxes/webhooks/{}", s(&f["inbox"], "name")), Default::default());
            }
        }
    });
    let load = move || {
        let n = name.get_untracked();
        if n.is_empty() {
            return;
        }
        let sn = since.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/webhooks/deliveries?inbox={}&limit=200{}", api::enc(&n), if sn == "all" { String::new() } else { format!("&since={sn}") })).await {
                let ds = v["deliveries"].as_array().cloned().unwrap_or_default();
                if selected.get_untracked().is_empty() || !ds.iter().any(|d| s(d, "id") == selected.get_untracked()) {
                    selected.set(ds.first().map(|d| s(d, "id")).unwrap_or_default());
                }
                list.set(ds);
                total.set(api::n(&v, "total"));
            }
        });
    };
    Effect::new(move |_| {
        name.track();
        since.track();
        selected.set(String::new());
        load();
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "webhook.delivery" {
            load();
            let id = selected.get_untracked();
            if !id.is_empty() {
                leptos::task::spawn_local(async move {
                    if let Ok(v) = api::get(&format!("/webhooks/deliveries/{id}")).await {
                        detail.set(v);
                    }
                });
            }
        }
        t
    });
    Effect::new(move |_| {
        let id = selected.get();
        if id.is_empty() {
            detail.set(Value::Null);
            return;
        }
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/webhooks/deliveries/{id}")).await {
                detail.set(v);
            }
        });
    });

    let current = move || sh.inboxes.get().into_iter().find(|v| s(&v["inbox"], "name") == name.get());
    let right = move || view! {
        <div class="anchor">
            <button class="btn" on:click=move |_| since_open.set(true)>{move || match since.get().as_str() { "15m" => "Last 15 min", "1h" => "Last hour", "1d" => "Last day", _ => "All kept" }}<span class="small">"▾"</span></button>
            <Pop open=since_open right=true>
                {[("15m", "Last 15 min"), ("1h", "Last hour"), ("1d", "Last day"), ("all", "All kept")].into_iter().map(|(v, l)| view! {
                    <div class="mi" on:click=move |_| since.set(v.to_string())><span>{l}</span>{move || (since.get() == v).then_some("✓")}</div> }).collect_view()}
            </Pop>
        </div>
    };

    view! {
        <div class="page">
            <Head active="deliveries" name=name right=right sh=sh/>
            <div class="body">
                <InboxList sh=sh selected=name tab="deliveries"/>
                <div class="list" style="width:528px">
                    {move || current().map(|v| {
                        let u = s(&v, "url");
                        let u2 = u.clone();
                        let n = s(&v["inbox"], "name");
                        view! {
                            <div style="padding:16px 24px;border-bottom:1px solid var(--rule)">
                                <div class="row"><span class="mono">{u}</span><button class="link" on:click=move |_| ui::copy(&u2, "URL")>"Copy"</button></div>
                                <div class="small t2" style="margin-top:4px">{format!("Set this as {n}'s webhook URL in your app. {} deliveries in the last hour.", api::n(&v, "deliveriesLastHour"))}</div>
                                {(v["expired"] == true).then(|| view! { <div class="small danger" style="margin-top:4px">"Expired: later deliveries get 410 Gone, so a leftover test configuration shows up as failures, not silence."</div> })}
                            </div>
                        }
                    })}
                    {move || {
                        let ds = list.get();
                        if ds.is_empty() {
                            let u = current().map(|v| s(&v, "url")).unwrap_or_default();
                            let u2 = u.clone();
                            return view! {
                                <div class="empty">
                                    <h3>{move || format!("Nothing delivered to {} yet", name.get())}</h3>
                                    <div class="small">{format!("Set {u} as the webhook URL of this customer in your app.")}</div>
                                    <button class="btn small" style="margin-top:12px" on:click=move |_| ui::copy(&u2, "URL")>"Copy URL"</button>
                                </div>
                            }.into_any();
                        }
                        // A long silence between deliveries is shown: often the app's circuit breaker.
                        let gaps: Vec<Option<(i64, i64)>> = ds.iter().enumerate().map(|(i, d)| ds.get(i + 1).and_then(|older| {
                            let (a, b) = (api::n(older, "at"), api::n(d, "at"));
                            (b - a > 10 * 60_000).then_some((a, b))
                        })).collect();
                        view! {
                            {ds.into_iter().zip(gaps).map(|(d, gap_after)| {
                                let id = s(&d, "id");
                                let (id2, id3) = (id.clone(), id.clone());
                                let (sig, cls) = sig_line(&d);
                                let extra = notes(&d);
                                let ans = s(&d, "answer");
                                let bad = !ans.starts_with('2');
                                view! {
                                    <div class="mrow" class:on=move || selected.get() == id2 on:click=move |_| selected.set(id3.clone())>
                                        <div class="row" style="align-items:flex-start">
                                            <span class="mono small cap" style="width:64px;padding-top:2px">{api::hms(api::n(&d, "at"))}</span>
                                            <div class="grow">
                                                <div class="mono">{format!("{}   {}", if s(&d, "eventType").is_empty() { s(&d, "method") } else { s(&d, "eventType") }, s(&d, "label"))}</div>
                                                <div class="small"><span class=cls>{sig}</span>{extra.into_iter().map(|e| view! { <span class="sep attn">{e}</span> }).collect_view()}</div>
                                            </div>
                                            <span class="mono" class:danger=bad>{if ans.is_empty() { "…".to_string() } else { ans }}</span>
                                        </div>
                                    </div>
                                    {gap_after.map(|(a, b)| view! {
                                        <div class="note" style="background:var(--soft);border-bottom:1px solid var(--rule);padding:10px 24px">
                                            <span class="attn">{format!("No deliveries for {} ({} → {})", api::span(b - a), api::hm(a), api::hm(b))}</span>
                                            <div>"If the app had events to send, it held them back: a circuit breaker after failures, or a stopped worker."</div>
                                        </div>
                                    })}
                                }
                            }).collect_view()}
                            <div class="note">"Signature and replay checks are Stuntman's; the inbox still answers what its behaviour says, so the app's retry logic is tested separately."</div>
                        }.into_any()
                    }}
                </div>
                <div class="pane pad">
                    {move || {
                        let d = detail.get();
                        if d.is_null() { return view! { <div class="empty"><h3>"Pick a delivery"</h3></div> }.into_any(); }
                        let sg = d["signature"].clone();
                        let attempts = d["attempts"].as_array().cloned().unwrap_or_else(|| vec![d.clone()]);
                        let headers = d["headers"].as_array().cloned().unwrap_or_default();
                        let body = s(&d, "body");
                        let pretty = serde_json::from_str::<Value>(&body).map(|v| serde_json::to_string_pretty(&v).unwrap_or(body.clone())).unwrap_or(body.clone());
                        let curl = format!("curl -X {} '{}' {} --data-binary '{}'", s(&d, "method"),
                            current().map(|v| s(&v, "url")).unwrap_or_default(),
                            headers.iter().filter(|h| !["host", "content-length", "connection"].contains(&h[0].as_str().unwrap_or("").to_lowercase().as_str())).map(|h| format!("-H '{}: {}'", h[0].as_str().unwrap_or(""), h[1].as_str().unwrap_or(""))).collect::<Vec<_>>().join(" "),
                            body.replace('\'', "'\\''"));
                        let trace = s(&d, "traceId");
                        let answered = { let a = s(&d, "answer"); if a.is_empty() { "still answering".to_string() } else if a.chars().all(|c| c.is_ascii_digit()) { format!("answered {a} in {} ms", api::n(&d, "answerMs")) } else { a } };
                        let last_ok = attempts.last().map(|a| s(a, "answer").starts_with('2')).unwrap_or(false);
                        let ua = s(&d, "userAgent");
                        view! {
                            <h2 class="mono" style="font-weight:500;margin:0">{format!("{}   {}", s(&d, "eventType"), s(&d, "label"))}</h2>
                            <div class="meta" style="margin-top:6px">{format!("{} from {}  ·  attempt {}  ·  {answered}", s(&d, "method"), if ua.is_empty() { s(&d, "remote") } else { format!("{ua} ({})", s(&d, "remote")) }, d["attempt"])}</div>
                            <div class="caps" style="margin-top:24px">"Signature"</div>
                            <div class="s12" style="margin-top:8px" class:ok=sg["result"] == "valid" class:danger=sg["result"] != "valid" && sg["result"] != "unchecked">
                                {match s(&sg, "result").as_str() { "valid" => "✓ ", "unchecked" => "", _ => "✕ " }}{s(&sg, "detail")}
                                {(!s(&sg, "header").is_empty()).then(|| format!("  ·  header {}", s(&sg, "header")))}
                            </div>
                            {(!s(&sg, "warning").is_empty()).then(|| view! { <div class="small attn" style="margin-top:4px">{format!("⚠ {}", s(&sg, "warning"))}</div> })}
                            <div class="small t2" style="margin-top:4px">
                                {sg["timestampAgeMs"].as_i64().map(|a| format!("{} timestamp {} old  ", if a < 300_000 { "✓" } else { "⚠" }, api::span(a)))}
                                {if d["duplicateOf"].is_null() { if d["attempt"].as_i64().unwrap_or(1) == 1 { "✓ first time this id is seen".to_string() } else { format!("retry: attempt {} of this id", d["attempt"]) } } else { format!("⚠ duplicate of the delivery at {}: customers that don't dedupe act twice", api::hms(d["duplicateOf"]["at"].as_i64().unwrap_or(0))) }}
                            </div>
                            {d["traps"].as_array().filter(|a| !a.is_empty()).map(|a| view! {
                                <div class="caps" style="margin-top:20px">"Traps"</div>
                                {a.iter().map(|t| view! { <div class="s12" style="margin-top:6px" class:ok=t["result"] == "passed" class:danger=t["result"] == "failed">
                                    {format!("{} · {} · {}", s(t, "trap").replace('_', " "), s(t, "result"), s(t, "detail"))}</div> }).collect_view()}
                            })}
                            <div class="caps" style="margin-top:20px">"Headers"</div>
                            <pre class="text" style="background:var(--subtle);border:0">{headers.iter().map(|h| format!("{}: {}", h[0].as_str().unwrap_or(""), h[1].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")}</pre>
                            <div class="caps" style="margin-top:20px">"Body"</div>
                            <pre class="text" style="background:var(--subtle);border:0;max-height:320px;overflow:auto">{if pretty.is_empty() { "(empty)".to_string() } else { pretty }}</pre>
                            <div class="row" style="margin-top:12px;gap:24px">
                                {(!trace.is_empty()).then(|| { let t = trace.clone(); view! { <button class="link" title="Copies the trace id" on:click=move |_| ui::copy(&t, "Trace id")>{format!("trace {} ↗", &trace[..8.min(trace.len())])}</button> } })}
                                <button class="link" on:click=move |_| ui::copy(&curl, "curl")>"Copy as curl"</button>
                            </div>
                            <div class="caps" style="margin-top:24px">"Attempts"</div>
                            {if attempts.len() <= 1 {
                                view! { <div class="s12" style="margin-top:8px">{format!("1 attempt  ·  {}", if last_ok { "answered with success, so the app did not retry".to_string() } else { "no retry yet".to_string() })}</div> }.into_any()
                            } else {
                                let mut prev = 0i64;
                                view! {
                                    <div style="margin-top:8px">{attempts.iter().map(|a| {
                                        let at = api::n(a, "at");
                                        let g = if prev == 0 { String::new() } else { format!("  ·  {}", gap(prev, at)) };
                                        prev = at;
                                        let ok = s(a, "answer").starts_with('2');
                                        view! { <div class="row s12" style="padding:3px 0"><span class="mono cap" style="width:70px">{api::hms(at)}</span>
                                            <span style="width:90px">{format!("attempt {}", a["attempt"])}</span><span class:ok=ok class:danger=!ok>{format!("{}{g}", s(a, "answer"))}</span></div> }
                                    }).collect_view()}</div>
                                    <div class="small t2" style="margin-top:6px">{if last_ok { "The app retried until the inbox said yes; the gaps are its backoff, measured." } else { "The app is still retrying, or gave up." }}</div>
                                }.into_any()
                            }}
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
