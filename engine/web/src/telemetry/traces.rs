//! T1 · Telemetry › Traces: the app's traces, with the spans that reached Stuntman drawn in the
//! provider's line colour, and each such span linked to the trip it caused.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use super::{line_colour, Head};
use crate::{
    api::{self, s},
    ui,
};

#[component]
pub fn Traces() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let sh = super::shared();
    let q = RwSignal::new(String::new());
    let service = RwSignal::new(String::new());
    let show = RwSignal::new(String::new()); // "", errors, slow, touching
    let list = RwSignal::new(Vec::<Value>::new());
    let all = RwSignal::new(Vec::<Value>::new());
    let selected = Memo::new(move |_| params.get().get("id").unwrap_or_default());
    let detail = RwSignal::new(Value::Null);
    let picked = RwSignal::new(String::new());

    Effect::new(move |_| {
        sh.tick.track();
        let (sv, sw, since) = (service.get(), show.get(), sh.since.get());
        leptos::task::spawn_local(async move {
            let mut qy = format!("/telemetry/traces?since={since}&limit=200");
            if !sv.is_empty() {
                qy += &format!("&service={}", api::enc(&sv));
            }
            match sw.as_str() {
                "errors" => qy += "&status=error",
                "slow" => qy += "&slower_than=1s",
                "touching" => qy += "&touching_provider=true",
                _ => {}
            }
            if let Ok(v) = api::get(&qy).await {
                list.set(v["traces"].as_array().cloned().unwrap_or_default());
            }
            if let Ok(v) = api::get(&format!("/telemetry/traces?since={since}&limit=500")).await {
                all.set(v["traces"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    let nav_first = nav.clone();
    Effect::new(move |_| {
        if selected.get().is_empty() {
            if let Some(f) = list.get().first() {
                nav_first(&format!("/telemetry/traces/{}", s(f, "traceId")), Default::default());
            }
        }
    });
    Effect::new(move |_| {
        let id = selected.get();
        sh.tick.track();
        if id.is_empty() {
            return;
        }
        leptos::task::spawn_local(async move {
            match api::get(&format!("/telemetry/traces/{id}")).await {
                Ok(v) => detail.set(v),
                Err(e) => detail.set(serde_json::json!({"error": e})),
            }
        });
    });

    let counts = move || {
        let a = all.get();
        let n = |f: &dyn Fn(&Value) -> bool| a.iter().filter(|t| f(t)).count();
        (n(&|t| t["error"] == true), n(&|t| t["durationMs"].as_i64().unwrap_or(0) > 1000), n(&|t| t["touchesProvider"] == true))
    };
    let filtered = move || {
        let t = q.get().to_lowercase();
        list.get().into_iter().filter(|x| t.is_empty() || s(x, "traceId").starts_with(&t) || s(x, "name").to_lowercase().contains(&t) || s(x, "statusCode").contains(&t)).collect::<Vec<_>>()
    };
    let nav_row = nav.clone();

    view! {
        <div class="page">
            <Head active="traces" sh=sh q=q placeholder="Trace id, route, status…"/>
            <div class="body">
                <div class="list narrow" style="display:flex;flex-direction:column">
                    <div class="section caps" style="padding-top:16px">"Service"</div>
                    {move || {
                        let svcs = sh.overview.get()["services"].as_object().cloned().unwrap_or_default();
                        svcs.into_iter().map(|(k, v)| { let (k2, k3) = (k.clone(), k.clone()); view! {
                            <div class="srow row" class:on=move || service.get() == k2 on:click=move |_| service.update(|x| *x = if *x == k3 { String::new() } else { k3.clone() })>
                                <span class="grow">{k.clone()}</span><span class="mono small cap">{v.to_string()}</span></div> } }).collect_view()
                    }}
                    <div class="section caps">"Show"</div>
                    {[("errors", "Errors"), ("slow", "Slower than 1 s"), ("touching", "Touching a provider")].into_iter().enumerate().map(|(i, (k, l))| view! {
                        <div class="srow row" class:on=move || show.get() == k on:click=move |_| show.update(|x| *x = if *x == k { String::new() } else { k.to_string() })>
                            <span class="grow">{l}</span>
                            <span class="mono small" class:danger=i == 0 class:attn=i == 1 class:cap=i == 2>{move || { let c = counts(); [c.0, c.1, c.2][i].to_string() }}</span></div>
                    }).collect_view()}
                    <div class="note">"Spans that reached Stuntman are drawn in the provider's line colour, so you see where your app waited on a provider."</div>
                    <div class="grow"></div>
                    <div style="border-top:1px solid var(--rule);padding:16px 24px">
                        <div class="caps">"Receiving OTLP"</div>
                        {move || { let ov = sh.overview.get(); let last = ov["lastData"].as_i64(); view! {
                            <div class="mono small" style="margin-top:8px">{s(&ov["endpoints"], "http")}</div>
                            <div class="small" class:ok=last.is_some() class:cap=last.is_none()>{match last { Some(l) => format!("{} services  ·  last data {}", api::n(&ov, "serviceCount"), api::ago(l)), None => "nothing received yet".to_string() }}</div>
                            {(!s(&ov, "listening").is_empty()).then(|| view! { <div class="small danger">{s(&ov, "listening")}</div> })}
                        } }}
                        <a class="link" href="/telemetry/settings" style="display:inline-block;margin-top:10px">"Exporter settings ›"</a>
                    </div>
                </div>
                <div class="list" style="width:528px">
                    {move || {
                        let rows = filtered();
                        if all.get().is_empty() {
                            let ov = sh.overview.get();
                            let env = super::env_lines(&ov);
                            let env2 = env.clone();
                            return view! {
                                <div class="empty" style="text-align:left">
                                    <h3>{format!("Nothing received in {} yet", s(&ov["settings"], "environment").to_uppercase())}</h3>
                                    <div>"Point your app's OpenTelemetry exporter at Stuntman. Any SDK works."</div>
                                    <pre class="text" style="background:var(--subtle);border:0">{env}</pre>
                                    <button class="btn small" on:click=move |_| ui::copy(&env2, "Settings")>"Copy"</button>
                                    <div class="small cap" style="margin-top:8px">"Any api-key header is accepted, so your APM key setting can stay."</div>
                                </div>
                            }.into_any();
                        }
                        let max = rows.iter().map(|t| api::n(t, "durationMs")).max().unwrap_or(1).max(1);
                        view! {
                            <div class="row caps" style="padding:14px 24px 10px;border-bottom:1px solid var(--rule)">
                                <span class="grow">"Trace"</span><span style="width:110px">"Duration"</span><span style="width:50px">"Spans"</span><span style="width:70px;text-align:right;color:var(--ink)">"When ↓"</span></div>
                            {rows.into_iter().map(|t| {
                                let id = s(&t, "traceId");
                                let id2 = id.clone();
                                let nav = nav_row.clone();
                                let d = api::n(&t, "durationMs");
                                let err = t["error"] == true;
                                let slow = d > 1000;
                                let col = if err { "var(--danger)" } else if slow { "var(--attention)" } else { "var(--border)" };
                                let sub = format!("{}  ·  {}{}{}", s(&t, "service"), if s(&t, "statusCode").is_empty() { if err { "error".into() } else { "ok".into() } } else { s(&t, "statusCode") },
                                    if slow { "  ·  slow" } else { "" }, if t["touchesProvider"] == true { format!("  ·  {} reached Stuntman", api::n(&t, "dubSpans").max(api::n(&t, "trips"))) } else { String::new() });
                                view! {
                                    <div class="mrow" class:on=move || selected.get() == id2 on:click=move |_| nav(&format!("/telemetry/traces/{id}"), Default::default())>
                                        <div class="row" style="align-items:flex-start">
                                            <div class="grow"><div class="mono">{s(&t, "name")}</div><div class="small" class:danger=err class:attn=slow && !err class:t2=!err && !slow>{sub}</div></div>
                                            <div style="width:110px"><div style="height:4px;background:var(--subtle);border-radius:2px;margin-top:6px"><div style=format!("height:4px;border-radius:2px;width:{}%;background:{col}", (d * 100 / max).max(2))></div></div>
                                                <div class="mono small t2" style="margin-top:6px">{format!("{d} ms")}</div></div>
                                            <span class="mono small" style="width:50px">{api::n(&t, "spans")}</span>
                                            <span class="mono small cap" style="width:70px;text-align:right">{api::hms(api::n(&t, "at"))}</span>
                                        </div>
                                    </div>
                                }
                            }).collect_view()}
                            <div class="note">{move || format!("Kept {} in {}. Nothing here goes to a real APM unless you turn forwarding on.", s(&sh.overview.get()["settings"]["signals"]["traces"], "keep"), s(&sh.overview.get()["settings"], "environment").to_uppercase())}</div>
                        }.into_any()
                    }}
                </div>
                <div class="pane pad">
                    {move || {
                        let t = detail.get();
                        if t.is_null() { return view! { <div class="empty"><h3>"Pick a trace"</h3></div> }.into_any(); }
                        if !s(&t, "error").is_empty() { return view! { <div class="empty"><h3>{s(&t, "error")}</h3></div> }.into_any(); }
                        let spans = t["spans"].as_array().cloned().unwrap_or_default();
                        let one_span = spans.len() == 1;
                        let total = t["durationMs"].as_f64().unwrap_or(1.0).max(0.001);
                        let root = spans.first().cloned().unwrap_or(Value::Null);
                        let code = root["attributes"]["http.response.status_code"].clone();
                        let tid = s(&t, "traceId");
                        let (tid2, tid3) = (tid.clone(), tid.clone());
                        let exc = spans.iter().find(|sp| !sp["exception"].is_null()).cloned();
                        view! {
                            <h2 class="mono" style="font-weight:500;margin:0">{s(&t, "name")}</h2>
                            <div class="meta" style="margin-top:6px">{format!("{}  ·  {:.0} ms{}  ·  trace {}…{}", s(&t, "service"), total, if code.is_null() { String::new() } else { format!("  ·  {}{}", if code.as_i64().unwrap_or(0) >= 500 { "error " } else { "" }, code) }, &tid[..8], &tid[tid.len() - 4..])}</div>
                            <div class="row" style="margin-top:12px">
                                <button class="btn small" on:click=move |_| ui::copy(&tid2, "Trace id")>"Copy trace id"</button>
                                <a class="btn small" href=format!("/telemetry/logs?trace={tid3}")>"Logs of this trace ›"</a>
                            </div>
                            <div class="caps" style="margin-top:24px">"Spans"</div>
                            <div style="margin-top:10px">
                                {spans.iter().cloned().map(|sp| {
                                    let off = sp["offsetMs"].as_f64().unwrap_or(0.0);
                                    let dur = sp["durationMs"].as_f64().unwrap_or(0.0);
                                    let depth = api::n(&sp, "depth").min(6);
                                    let dub = sp["reachedStuntman"] == true;
                                    let err = sp["status"] == 2;
                                    let col = if dub { line_colour(&sp) } else if err { "var(--danger)" } else if depth == 0 { "var(--ink)" } else { "var(--border)" };
                                    let sid = s(&sp, "spanId");
                                    let (sid2, sid3) = (sid.clone(), sid.clone());
                                    let code = sp["attributes"]["http.response.status_code"].clone();
                                    let note = if dub { format!("{}{}", if code.is_null() { String::new() } else { code.to_string() }, if !sp["trip"].is_null() { format!("  ·  {}", s(&sp["trip"], "status")) } else { String::new() }) } else if dur >= 1.0 { format!("{dur:.0} ms") } else { String::new() };
                                    view! {
                                        <div style=format!("padding:6px 0 6px {}px;cursor:pointer;{}", 12 * depth, "") class:on=move || picked.get() == sid2
                                            on:click=move |_| picked.set(sid3.clone())>
                                            <div class="mono s12" class:danger=err && !dub>{s(&sp, "name")}</div>
                                            <div class="row" style="gap:6px;margin-top:3px">
                                                <div style=format!("margin-left:{}%;width:{}%;height:{}px;background:{col};border-radius:1px;min-width:3px", off / total * 100.0 * 0.8, (dur / total * 100.0 * 0.8).max(0.4), if depth == 0 { 6 } else { 5 })></div>
                                                <span class="small" class:attn=dub class:cap=!dub>{note}</span>
                                            </div>
                                        </div>
                                    }
                                }).collect_view()}
                                {t["unmatchedTrips"].as_array().filter(|a| !a.is_empty()).map(|a| view! {
                                    <div class="small cap" style="margin-top:6px">{format!("{} trip{} of this trace had no matching client span: ", a.len(), if a.len() == 1 { "" } else { "s" })}
                                        {a.iter().map(|tr| view! { <a class="link" href=s(tr, "link")>{s(tr, "label")}</a> }).collect_view()}</div>
                                })}
                            </div>
                            <div class="small cap" style="margin-top:8px">"Coloured spans reached Stuntman. Click one to see what answered it."</div>
                            {exc.map(|e| { let st = s(&e["exception"], "exception.stacktrace"); let st2 = st.clone(); view! {
                                <div class="caps" style="margin-top:20px">"Exception"</div>
                                <div class="s12" style="margin-top:6px">{s(&e, "name")}</div>
                                <pre class="text danger" style="border-color:var(--rule)">{if st.is_empty() { format!("{}: {}", s(&e["exception"], "exception.type"), s(&e["exception"], "exception.message")) } else { st }}</pre>
                                <button class="link plain" on:click=move |_| ui::copy(&st2, "Stack trace")>"Copy stack trace"</button>
                            } })}
                            {move || {
                                let id = picked.get();
                                let sp = spans.iter().find(|x| s(x, "spanId") == id).cloned()?;
                                let trip = sp["trip"].clone();
                                Some(view! {
                                    <div class="caps" style="margin-top:20px">"Selected span"</div>
                                    <table class="kv" style="margin-top:6px">
                                        <tr><td>"span"</td><td>{format!("{}  ·  {}", s(&sp, "name"), s(&sp, "service"))}</td></tr>
                                        <tr><td>"answered by"</td><td>{if trip.is_null() { if sp["reachedStuntman"] == true { format!("Stuntman ({})", s(&sp, "host")) } else { "your app or a real host".to_string() } } else { format!("Stuntman · {}", s(&trip, "label")) }}</td></tr>
                                        <tr><td>"status"</td><td>{let c = sp["attributes"]["http.response.status_code"].clone(); if c.is_null() { match api::n(&sp, "status") { 2 => "error".to_string(), 1 => "ok".to_string(), _ => "unset".to_string() } } else { c.to_string() }}</td></tr>
                                        <tr><td>"duration"</td><td>{format!("{:.1} ms", sp["durationMs"].as_f64().unwrap_or(0.0))}</td></tr>
                                        {(!trip.is_null()).then(|| view! { <tr><td>"the trip"</td><td><a class="link" href=s(&trip, "link")>{if s(&trip, "module") == "email" { "Open in Notifications ›" } else { "Open in Inboxes ›" }}</a></td></tr> })}
                                        {sp["attributes"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| view! { <tr><td>{k}</td><td class="mono">{v.as_str().map(String::from).unwrap_or(v.to_string())}</td></tr> }).collect_view()}
                                    </table>
                                })
                            }}
                            {one_span.then(|| view! { <div class="small cap" style="margin-top:12px">"1 span  ·  nothing reached Stuntman, so nothing opens in Notifications or Inboxes."</div> })}
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
