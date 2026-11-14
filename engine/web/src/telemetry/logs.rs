//! T2 · Telemetry › Logs: lines as the app wrote them, each with the trace it came from.

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use serde_json::Value;

use super::Head;
use crate::{
    api::{self, s},
    ui,
};

const LEVELS: [(&str, &str); 4] = [("ERROR", "Error"), ("WARN", "Warning"), ("INFO", "Info"), ("DEBUG", "Debug")];

fn level_class(l: &str) -> &'static str {
    match l {
        "ERROR" | "FATAL" => "danger",
        "WARN" => "attn",
        _ => "t2",
    }
}

#[component]
pub fn Logs() -> impl IntoView {
    let params = use_params_map();
    let query = use_query_map();
    let sh = super::shared();
    let q = RwSignal::new(String::new());
    let trace = RwSignal::new(query.get_untracked().get("trace").unwrap_or_default());
    let levels = RwSignal::new(Vec::<String>::new());
    let services = RwSignal::new(Vec::<String>::new());
    let list = RwSignal::new(Vec::<Value>::new());
    let total = RwSignal::new(0i64);
    let facets = RwSignal::new(Value::Null);
    let selected = RwSignal::new(params.get_untracked().get("id").unwrap_or_default());
    let detail = RwSignal::new(Value::Null);
    let around = RwSignal::new(Value::Null);

    Effect::new(move |_| {
        sh.tick.track();
        let (t, lv, sv, tr, since) = (q.get(), levels.get(), services.get(), trace.get(), sh.since.get());
        leptos::task::spawn_local(async move {
            let mut qy = format!("/telemetry/logs?limit=300&since={}", if tr.is_empty() { since } else { "7d".into() });
            if !t.trim().is_empty() {
                qy += &format!("&text={}", api::enc(t.trim()));
            }
            if !lv.is_empty() {
                qy += &format!("&level={}", lv.join(","));
            }
            if !sv.is_empty() {
                qy += &format!("&service={}", api::enc(&sv.join(",")));
            }
            if !tr.is_empty() {
                qy += &format!("&trace_id={tr}");
            }
            if let Ok(v) = api::get(&qy).await {
                let ls = v["logs"].as_array().cloned().unwrap_or_default();
                if selected.get_untracked().is_empty() {
                    selected.set(ls.first().map(|l| s(l, "id")).unwrap_or_default());
                }
                list.set(ls);
                total.set(api::n(&v, "total"));
                facets.set(v["facets"].clone());
            }
        });
    });
    Effect::new(move |_| {
        let id = selected.get();
        around.set(Value::Null);
        if id.is_empty() {
            detail.set(Value::Null);
            return;
        }
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/telemetry/logs/{id}")).await {
                detail.set(v);
            }
        });
    });
    let toggle = |sig: RwSignal<Vec<String>>, v: String| sig.update(|x| if let Some(i) = x.iter().position(|y| *y == v) { x.remove(i); } else { x.push(v); });

    view! {
        <div class="page">
            <Head active="logs" sh=sh q=q placeholder="Text, trace id, attribute…"/>
            <div class="body">
                <div class="list narrow">
                    <div class="section caps" style="padding-top:16px">"Level"</div>
                    {LEVELS.iter().map(|(k, l)| { let k = k.to_string(); let (k2, k3, k4) = (k.clone(), k.clone(), k.clone()); view! {
                        <div class="srow row" class:on=move || levels.get().contains(&k2) on:click=move |_| toggle(levels, k3.clone())>
                            <span class="grow">{*l}</span>
                            <span class=format!("mono small {}", level_class(&k))>{move || { let n = facets.get()["levels"][&k4].as_i64().unwrap_or(0); if k4 == "DEBUG" && n == 0 { "off".to_string() } else { n.to_string() } }}</span></div>
                    } }).collect_view()}
                    <div class="section caps">"Service"</div>
                    {move || facets.get()["services"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| { let (k2, k3) = (k.clone(), k.clone()); view! {
                        <div class="srow row" class:on=move || services.get().contains(&k2) on:click=move |_| toggle(services, k3.clone())>
                            <span class="grow">{k.clone()}</span><span class="mono small cap">{v.to_string()}</span></div> } }).collect_view()}
                    <div class="note">"Logs keep the trace id the app sent, so every line opens its trace."</div>
                </div>
                <div class="list" style="width:528px">
                    {move || (!trace.get().is_empty() || !levels.get().is_empty() || !services.get().is_empty()).then(|| view! {
                        <div class="row small" style="padding:10px 24px;border-bottom:1px solid var(--rule);flex-wrap:wrap">
                            {(!trace.get().is_empty()).then(|| view! { <span class="chip">{format!("trace {}…", &trace.get()[..8.min(trace.get().len())])}<button on:click=move |_| trace.set(String::new())>"×"</button></span> })}
                            {(!levels.get().is_empty()).then(|| view! { <span class="chip">{levels.get().join(", ")}<button on:click=move |_| levels.set(vec![])>"×"</button></span> })}
                            {(!services.get().is_empty()).then(|| view! { <span class="chip">{services.get().join(", ")}<button on:click=move |_| services.set(vec![])>"×"</button></span> })}
                            <span>{move || format!("{} lines", total.get())}</span>
                            <button class="link" on:click=move |_| { trace.set(String::new()); levels.set(vec![]); services.set(vec![]); }>"Clear filters"</button>
                        </div>
                    })}
                    {move || {
                        let ls = list.get();
                        if ls.is_empty() {
                            let ov = sh.overview.get();
                            let logs_off = ov["settings"]["signals"]["logs"]["on"] == false;
                            return if logs_off {
                                view! { <div class="empty"><h3>"Logs are off"</h3><div>"Stuntman answers 200 to /v1/logs and drops the batch, so the app never blocks or retries."</div><a class="link" href="/telemetry/settings">"Exporter settings ›"</a></div> }.into_any()
                            } else if api::n(&ov, "traces") > 0 && q.get().is_empty() {
                                view! { <div class="empty"><h3>"Traces are arriving, logs are not"</h3><div>"Most SDKs send logs only when the log exporter is on. Set it and restart the app."</div>
                                    <pre class="text" style="background:var(--subtle);border:0">"OTEL_LOGS_EXPORTER=otlp"</pre><button class="btn small" on:click=|_| ui::copy("OTEL_LOGS_EXPORTER=otlp", "Setting")>"Copy"</button></div> }.into_any()
                            } else {
                                view! { <div class="empty"><h3>"No log lines match"</h3></div> }.into_any()
                            };
                        }
                        view! {
                            <div class="row caps" style="padding:14px 24px 10px;border-bottom:1px solid var(--rule)"><span style="width:100px">"Time"</span><span style="width:56px">"Level"</span><span>"Message"</span></div>
                            {ls.into_iter().map(|l| {
                                let id = s(&l, "id");
                                let (id2, id3) = (id.clone(), id.clone());
                                let lv = s(&l, "level");
                                let t = api::n(&l, "timeNs");
                                let ms = (t / 1_000_000) % 1000;
                                view! {
                                    <div class="mrow row" style="padding:9px 24px;align-items:flex-start" class:on=move || selected.get() == id2 on:click=move |_| selected.set(id3.clone())>
                                        <span class="mono small cap" style="width:100px;flex:none">{format!("{}.{ms:03}", api::hms(t / 1_000_000))}</span>
                                        <span class=format!("mono small {}", level_class(&lv)) style="width:56px;flex:none">{lv.clone()}</span>
                                        <span class="s12" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{s(&l, "message")}</span>
                                    </div>
                                }
                            }).collect_view()}
                            <div class="note">{move || format!("Newest first. {} lines in the window; narrow the filters for older ones.", total.get())}</div>
                        }.into_any()
                    }}
                </div>
                <div class="pane pad">
                    {move || {
                        let l = detail.get();
                        if l.is_null() { return view! { <div class="empty"><h3>"Pick a line"</h3></div> }.into_any(); }
                        let lv = s(&l, "level");
                        let tid = s(&l, "traceId");
                        let attrs = l["attributes"].as_object().cloned().unwrap_or_default();
                        let msg = s(&l, "message");
                        let structured = serde_json::from_str::<Value>(&msg).ok().filter(|v| v.is_object());
                        let exc = attrs.get("exception.stacktrace").or(attrs.get("exception.message")).map(|v| v.as_str().unwrap_or("").to_string());
                        let id = s(&l, "id");
                        let t = api::n(&l, "timeNs");
                        view! {
                            <div class="row"><span class=format!("caps {}", level_class(&lv)) style="color:inherit">{lv.clone()}</span>
                                <span class="mono small cap">{format!("{}.{:03}  ·  {}", api::hms(t / 1_000_000), (t / 1_000_000) % 1000, s(&l, "service"))}</span></div>
                            {match &structured {
                                Some(v) => view! { <pre class="text">{serde_json::to_string_pretty(v).unwrap_or_default()}</pre><div class="small cap">"When the body is JSON, the detail shows it in full, keys in the order the app sent them."</div> }.into_any(),
                                None => view! { <div style="font-size:15px;margin-top:8px;white-space:pre-wrap;word-break:break-word">{msg.chars().take(65536).collect::<String>()}</div> }.into_any(),
                            }}
                            <div class="row" style="margin-top:12px">
                                {(!tid.is_empty()).then(|| view! { <a class="btn small" href=format!("/telemetry/traces/{tid}")>"Open trace ›"</a> })}
                                <button class="btn small" on:click=move |_| { let id = id.clone(); leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/telemetry/logs/{id}/around?before=10&after=10")).await { around.set(v); } }); }>"Lines around this one"</button>
                            </div>
                            <div class="caps" style="margin-top:22px">"Attributes"</div>
                            <table class="kv">
                                {(!tid.is_empty()).then(|| view! { <tr><td>"trace"</td><td class="mono"><a class="link" href=format!("/telemetry/traces/{tid}")>{format!("{}…{}  ›", &tid[..8], &tid[tid.len() - 4..])}</a></td></tr> })}
                                {(!s(&l, "spanId").is_empty()).then(|| view! { <tr><td>"span"</td><td class="mono">{s(&l, "spanId")}</td></tr> })}
                                {attrs.into_iter().filter(|(k, _)| k != "exception.stacktrace").map(|(k, v)| { let err = k == "exception.type"; view! { <tr><td>{k}</td><td class="mono" class:danger=err>{v.as_str().map(String::from).unwrap_or(v.to_string())}</td></tr> } }).collect_view()}
                            </table>
                            {exc.map(|e| view! { <div class="caps" style="margin-top:20px">"Exception"</div><pre class="text danger">{e}</pre> })}
                            {move || {
                                let a = around.get();
                                (!a.is_null()).then(|| view! {
                                    <div class="caps" style="margin-top:20px">"Lines around this one"</div>
                                    <div style="margin-top:6px">
                                        {a["before"].as_array().cloned().unwrap_or_default().into_iter().chain(std::iter::once(a["line"].clone())).chain(a["after"].as_array().cloned().unwrap_or_default()).map(|x| {
                                            let me = x["id"] == a["line"]["id"];
                                            view! { <div class="mono small" style=if me { "background:var(--attention-fill);padding:2px 4px" } else { "padding:2px 4px" }>
                                                {format!("{}  {:5}  {}  {}", api::hms(api::n(&x, "at")), s(&x, "level"), s(&x, "service"), s(&x, "message"))}</div> }
                                        }).collect_view()}
                                    </div>
                                })
                            }}
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
