//! T4 · Telemetry › Metrics: a metric per minute, broken down by an attribute, with the calls that
//! went to Stuntman told apart from calls to real providers; bars open their slowest trace.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use super::Head;
use crate::{
    api::{self, s},
    ui,
};

const BREAKDOWNS: [&str; 5] = ["server.address", "http.response.status_code", "http.request.method", "service.name", ""];

fn default_stat(kind: &str) -> &'static str {
    match kind {
        "histogram" => "p95",
        "counter" => "rate",
        _ => "last",
    }
}

fn fmt(v: f64, unit: &str) -> String {
    match unit {
        "s" => if v < 1.0 { format!("{:.0} ms", v * 1000.0) } else { format!("{v:.2} s") },
        "ms" => format!("{v:.0} ms"),
        "By" => api::kb(v as i64),
        _ => if v.fract() == 0.0 { format!("{v:.0}") } else { format!("{v:.2}") },
    }
}

/// Bars by step; stacked when several series are given (e.g. ok / 5xx). Click → on_bar(t).
#[component]
fn Bars(series: Vec<(String, &'static str, Vec<(i64, f64)>)>, unit: String, #[prop(into)] on_bar: Callback<i64>, #[prop(optional_no_strip)] band: Option<(i64, i64, String)>) -> impl IntoView {
    let steps: Vec<i64> = series.first().map(|s| s.2.iter().map(|p| p.0).collect()).unwrap_or_default();
    let totals: Vec<f64> = steps.iter().enumerate().map(|(i, _)| series.iter().map(|s| s.2.get(i).map(|p| p.1).unwrap_or(0.0)).sum()).collect();
    let max = totals.iter().cloned().fold(0.0_f64, f64::max).max(1e-9);
    let (w, h, left) = (460.0, 170.0, 34.0);
    let n = steps.len().max(1) as f64;
    let bw = ((w - left) / n * 0.72).max(2.0);
    let label = |t: i64| api::hm(t);
    view! {
        <svg width="100%" viewBox=format!("0 0 {} {}", w, h + 24.0) style="display:block">
            {band.clone().and_then(|(a, b, label)| {
                let st = steps.get(1).zip(steps.first()).map(|(b, a)| b - a).unwrap_or(60_000);
                let i0 = steps.iter().position(|t| *t + st > a)?;
                let i1 = steps.iter().rposition(|t| *t < b).unwrap_or(i0);
                let x0 = left + (w - left) / n * i0 as f64;
                let x1 = left + (w - left) / n * (i1 + 1) as f64;
                Some(view! { <rect x=x0 y="0" width=x1 - x0 height=h fill="var(--attention-fill)"/>
                    <text x=x0 + 3.0 y="11" font-size="10" fill="var(--attention)" font-family="Archivo">{label}</text> })
            })}
            <line x1=left y1=h x2=w y2=h stroke="var(--rule)"/>
            <line x1=left y1=h / 2.0 x2=w y2=h / 2.0 stroke="var(--rule)"/>
            <text x="0" y=h / 2.0 + 4.0 font-size="10" font-family="JetBrains Mono" fill="var(--caption)">{fmt(max / 2.0, &unit)}</text>
            <text x="0" y="12" font-size="10" font-family="JetBrains Mono" fill="var(--caption)">{fmt(max, &unit)}</text>
            {steps.iter().enumerate().map(|(i, t)| {
                let x = left + (w - left) / n * i as f64 + 2.0;
                let mut y = h;
                let t = *t;
                let segs = series.iter().map(|(_, col, pts)| {
                    let v = pts.get(i).map(|p| p.1).unwrap_or(0.0);
                    let bh = v / max * (h - 16.0);
                    y -= bh;
                    let st = steps.get(1).zip(steps.first()).map(|(b, a)| b - a).unwrap_or(60_000);
                    let in_band = band.as_ref().map(|(a, b, _)| t + st > *a && t < *b).unwrap_or(false);
                    let fill = if in_band && *col == "var(--raised-dark)" { "var(--attention)" } else { *col };
                    view! { <rect x=x y=y width=bw height=bh.max(0.0) fill=fill/> }
                }).collect_view();
                view! { <g style="cursor:pointer" on:click=move |_| on_bar.run(t)>{segs}<title>{format!("{}  ·  {}", api::hm(t), fmt(totals[i], &unit))}</title></g> }
            }).collect_view()}
            {steps.first().map(|t| view! { <text x=left y=h + 16.0 font-size="10" font-family="JetBrains Mono" fill="var(--caption)">{label(*t)}</text> })}
            {steps.get(steps.len() / 2).map(|t| view! { <text x=(w + left) / 2.0 - 14.0 y=h + 16.0 font-size="10" font-family="JetBrains Mono" fill="var(--caption)">{label(*t)}</text> })}
            {steps.last().map(|t| view! { <text x=w - 30.0 y=h + 16.0 font-size="10" font-family="JetBrains Mono" fill="var(--caption)">{label(*t)}</text> })}
        </svg>
    }
}

fn points(series: &Value) -> Vec<(i64, f64)> {
    series["points"].as_array().map(|a| a.iter().map(|p| (api::n(p, "t"), p["v"].as_f64().unwrap_or(0.0))).collect()).unwrap_or_default()
}

#[component]
pub fn Metrics() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let sh = super::shared();
    let q = RwSignal::new(String::new());
    let list = RwSignal::new(Vec::<Value>::new());
    let name = Memo::new(move |_| params.get().get("name").unwrap_or_default());
    let by = RwSignal::new("server.address".to_string());
    let chart = RwSignal::new(Value::Null);
    let breakdown = RwSignal::new(Value::Null);
    let server = RwSignal::new(Value::Null);
    let why = RwSignal::new(Value::Null);
    let other = RwSignal::new(String::new());
    let other_chart = RwSignal::new(Value::Null);
    let picking = RwSignal::new(false);
    let compare = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        let (o, since) = (other.get(), sh.since.get());
        sh.tick.track();
        if o.is_empty() { other_chart.set(Value::Null); return; }
        let kind = list.get_untracked().iter().find(|m| s(m, "name") == o).map(|m| s(m, "kind")).unwrap_or("histogram".into());
        let step = match since.as_str() { "5m" => "20s", "1h" => "5m", "24h" => "1h", _ => "1m" };
        leptos::task::spawn_local(async move {
            other_chart.set(api::get(&format!("/telemetry/metrics/{o}?stat={}&since={since}&step={step}", default_stat(&kind))).await.unwrap_or(Value::Null));
        });
    });
    let err = RwSignal::new(String::new());

    Effect::new(move |_| {
        sh.tick.track();
        let t = q.get();
        leptos::task::spawn_local(async move {
            let qy = if t.trim().is_empty() { "/telemetry/metrics".to_string() } else { format!("/telemetry/metrics?text={}", api::enc(t.trim())) };
            if let Ok(v) = api::get(&qy).await {
                list.set(v["metrics"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    let nav_first = nav.clone();
    Effect::new(move |_| {
        if name.get().is_empty() {
            let l = list.get();
            if let Some(f) = l.iter().find(|m| s(m, "category") == "Provider calls").or(l.first()) {
                nav_first(&format!("/telemetry/metrics/{}", s(f, "name")), Default::default());
            }
        }
    });
    Effect::new(move |_| {
        sh.tick.track();
        let (n, b, since) = (name.get(), by.get(), sh.since.get());
        if n.is_empty() {
            return;
        }
        let kind = list.get_untracked().iter().find(|m| s(m, "name") == n).map(|m| s(m, "kind")).unwrap_or("histogram".into());
        let stat = default_stat(&kind);
        let step = match since.as_str() { "5m" => "20s", "1h" => "5m", "24h" => "1h", _ => "1m" };
        leptos::task::spawn_local(async move {
            match api::get(&format!("/telemetry/metrics/{n}?stat={stat}&since={since}&step={step}")).await {
                Ok(v) => { chart.set(v); err.set(String::new()); }
                Err(e) => { chart.set(Value::Null); err.set(e); }
            }
            if let Ok(v) = api::get(&format!("/telemetry/metrics/{n}?stat={stat}&since={since}&step={since}&by={}", api::enc(&b))).await {
                breakdown.set(v);
            }
            why.set(api::get(&format!("/telemetry/metrics/{n}/why?since={since}&step={step}&by={}", api::enc(&b))).await.unwrap_or(Value::Null));
            // The companion chart: requests to the app per minute, ok and 5xx.
            server.set(api::get(&format!("/telemetry/metrics/http.server.request.duration?stat=count&by=http.response.status_code&since={since}&step={step}")).await.unwrap_or(Value::Null));
        });
    });
    let nav_bar = nav.clone();
    let open_slowest = move |t: i64| {
        let n = name.get_untracked();
        let nav = nav_bar.clone();
        leptos::task::spawn_local(async move {
            match api::get(&format!("/telemetry/metrics/{n}/exemplars?at={t}&step=1m")).await {
                Ok(v) => match v["exemplars"].as_array().and_then(|a| a.first()).map(|e| s(e, "traceId")) {
                    Some(tid) if !tid.is_empty() => nav(&format!("/telemetry/traces/{tid}"), Default::default()),
                    _ => ui::toast("This bar has no trace examples: the app's SDK did not attach exemplars."),
                },
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let nav_list = nav.clone();

    view! {
        <div class="page">
            <Head active="metrics" sh=sh q=q placeholder="Metric name or attribute…"/>
            <div class="body">
                <div class="list narrow">
                    {move || {
                        let l = list.get();
                        if l.is_empty() { return view! { <div class="note" style="padding-top:20px">"No metrics yet."</div> }.into_any(); }
                        let nav = nav_list.clone();
                        ["Provider calls", "The app", "Runtime", "Custom"].into_iter().filter_map(|cat| {
                            let items: Vec<Value> = l.iter().filter(|m| s(m, "category") == cat).cloned().collect();
                            (!items.is_empty()).then(|| { let nav = nav.clone(); view! {
                                <div class="section caps">{cat}</div>
                                {items.into_iter().map(|m| { let n = s(&m, "name"); let (n2, n3) = (n.clone(), n.clone()); let nav = nav.clone();
                                    let stopped = api::now() as i64 - api::n(&m, "last") > 5 * 60_000;
                                    view! { <div class="srow" class:on=move || name.get() == n2 on:click=move |_| nav(&format!("/telemetry/metrics/{n3}"), Default::default())>
                                        <div class="n" style="font-size:12px">{n.clone()}</div>
                                        {stopped.then(|| view! { <div class="d attn">{format!("nothing since {}", api::hm(api::n(&m, "last")))}</div> })}</div> } }).collect_view()}
                            } })
                        }).collect_view().into_any()
                    }}
                    <div class="note">{move || format!("{} series from {} services. Names follow the OpenTelemetry semantic conventions.", list.get().iter().map(|m| api::n(m, "series")).sum::<i64>(), api::n(&sh.overview.get(), "serviceCount"))}</div>
                </div>
                <div class="list" style="width:528px;padding:16px 24px">
                    {let open_slowest = open_slowest.clone(); move || {
                        let c = chart.get();
                        if c.is_null() {
                            let ov = sh.overview.get();
                            return if ov["settings"]["signals"]["metrics"]["on"] == false {
                                view! { <div class="empty" style="text-align:left"><h3 style="color:var(--attention)">{format!("Metrics are off for {}", s(&ov["settings"], "environment").to_uppercase())}</h3>
                                    <div>{format!("Stuntman answers 200 to /v1/metrics and drops the batch, so the app never blocks or retries. {} data points dropped in the last 15 min.", api::thousands(api::n(&ov["dropped"], "metrics")))}</div>
                                    <a class="link" href="/telemetry/settings">"Exporter settings ›"</a></div> }.into_any()
                            } else if list.get().is_empty() && api::n(&ov, "traces") + api::n(&ov, "logs") == 0 {
                                view! { <div class="empty"><h3>{format!("Nothing received in {} yet", s(&ov["settings"], "environment").to_uppercase())}</h3><div>"Point your app's OpenTelemetry exporter at Stuntman; metrics, traces and logs share the endpoint."</div>
                                    <pre class="text" style="background:var(--subtle);border:0">{super::env_lines(&ov)}</pre></div> }.into_any()
                            } else if list.get().is_empty() {
                                view! { <div class="empty" style="text-align:left"><h3>"Traces and logs are arriving, metrics are not"</h3><div>"Turn on the metrics exporter and restart the app. Metrics are sent every 60 s by default."</div>
                                    <pre class="text" style="background:var(--subtle);border:0">"OTEL_METRICS_EXPORTER=otlp   OTEL_METRIC_EXPORT_INTERVAL=10000"</pre>
                                    <button class="link" on:click=|_| ui::copy("OTEL_METRICS_EXPORTER=otlp\nOTEL_METRIC_EXPORT_INTERVAL=10000", "Settings")>"Copy"</button></div> }.into_any()
                            } else {
                                view! { <div class="empty"><h3>{move || if err.get().is_empty() { "Loading…".to_string() } else { err.get() }}</h3></div> }.into_any()
                            };
                        }
                        let unit = s(&c, "unit");
                        let all: Vec<(i64, f64)> = c["series"].as_array().and_then(|a| a.first()).map(points).unwrap_or_default();
                        let title = match (s(&c, "kind").as_str(), s(&c, "stat").as_str()) { ("histogram", st) => format!("{}  ·  {st}{}", name.get(), if unit == "s" || unit == "ms" { " duration" } else { "" }), (_, "rate") => format!("{}  ·  per minute", name.get()), _ => format!("{}  ·  last value", name.get()) };
                        let srv = server.get();
                        let (ok, bad): (Vec<Value>, Vec<Value>) = srv["series"].as_array().cloned().unwrap_or_default().into_iter().partition(|x| s(x, "key").parse::<i64>().map(|c| c < 500).unwrap_or(true));
                        let sum = |v: &[Value]| -> Vec<(i64, f64)> { let mut out: Vec<(i64, f64)> = v.first().map(points).unwrap_or_default().iter().map(|p| (p.0, 0.0)).collect(); for x in v { for (i, p) in points(x).iter().enumerate() { if let Some(o) = out.get_mut(i) { o.1 += p.1; } } } out };
                        let (okp, badp) = (sum(&ok), sum(&bad));
                        view! {
                            <div class="row"><span style="font-weight:500">{title}</span><span class="small cap">"by step, all hosts"</span></div>
                            {
                                let w = why.get();
                                let band = w["jump"].as_object().map(|j| (j["from"].as_i64().unwrap_or(0), j["to"].as_i64().unwrap_or(0), if w["ruleOn"] == true { "Rule on".to_string() } else { "Jump".to_string() }));
                                view! { <Bars series=vec![("all".into(), "var(--raised-dark)", all)] unit=unit on_bar=open_slowest.clone() band=band/> }
                            }
                            {(!okp.is_empty() && name.get() != "http.server.request.duration").then(|| view! {
                                <div class="row" style="margin-top:20px"><span style="font-weight:500">"Requests to the app  ·  per step"</span>
                                    <span class="small"><span style="color:var(--raised-dark)">"■"</span>" ok  "<span class="danger">"■"</span>" 5xx"</span></div>
                                <Bars series=vec![("ok".into(), "var(--raised-dark)", okp), ("5xx".into(), "var(--danger)", badp)] unit=String::new() on_bar=move |_| {}/>
                            })}
                            {move || {
                                let oc = other_chart.get();
                                (!oc.is_null()).then(|| {
                                    let pts: Vec<(i64, f64)> = oc["series"].as_array().and_then(|a| a.first()).map(points).unwrap_or_default();
                                    view! { <div class="row" style="margin-top:20px"><span style="font-weight:500">{format!("{}  ·  {}", other.get(), s(&oc, "stat"))}</span>
                                        <button class="link plain" on:click=move |_| other.set(String::new())>"×"</button></div>
                                        <Bars series=vec![("other".into(), "var(--line-blue)", pts)] unit=s(&oc, "unit") on_bar=move |_| {}/> }
                                })
                            }}
                            <div class="small t2" style="margin-top:12px">"Each bar keeps the trace examples the app sent: click a bar to open its slowest trace."</div>
                            <div class="row" style="margin-top:12px;gap:28px">
                                <div class="anchor"><button class="link" on:click=move |_| picking.set(true)>"+ Compare with another metric"</button>
                                    <crate::ui::Pop open=picking>
                                        {move || list.get().into_iter().filter(|m| s(m, "name") != name.get()).map(|m| { let n = s(&m, "name"); let n2 = n.clone(); view! { <div class="mi mono s12" on:click=move |_| other.set(n2.clone())>{n}</div> } }).collect_view()}
                                    </crate::ui::Pop></div>
                                <button class="link" on:click=move |_| {
                                    let w = why.get_untracked();
                                    let n = name.get_untracked();
                                    let b = by.get_untracked();
                                    let now = api::now() as i64;
                                    let (from, to) = match w["jump"].as_object() { Some(j) => (j["from"].as_i64().unwrap_or(now), j["to"].as_i64().unwrap_or(now)), None => (now - 5 * 60_000, now) };
                                    let start = now - 15 * 60_000;
                                    leptos::task::spawn_local(async move {
                                        match api::get(&format!("/telemetry/metrics/{n}/compare?before={start}..{from}&after={from}..{to}&stat=p95&by={}", api::enc(&b))).await {
                                            Ok(v) => compare.set(v),
                                            Err(e) => ui::toast_err(e),
                                        }
                                    });
                                }>{move || if why.get()["ruleOn"] == true { "+ Before and after a rule" } else { "+ Before and after" }}</button>
                            </div>
                            {move || {
                                let c = compare.get();
                                (!c.is_null()).then(|| { let unit = s(&chart.get(), "unit"); view! {
                                    <table class="kv" style="margin-top:12px">
                                        <tr><td></td><td class="caps">"Before"</td><td class="caps">"During"</td><td class="caps">"Change"</td></tr>
                                        {c["changes"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let ch = x["change"].as_f64(); let up = ch.map(|v| v > 0.2).unwrap_or(false); view! {
                                            <tr><td class="mono">{s(&x, "key")}</td><td class="mono">{x["before"].as_f64().map(|v| fmt(v, &unit)).unwrap_or("–".into())}</td>
                                                <td class="mono">{x["after"].as_f64().map(|v| fmt(v, &unit)).unwrap_or("–".into())}</td>
                                                <td class="mono" class:danger=up>{ch.map(|v| format!("{:+.0} %", v * 100.0)).unwrap_or("–".into())}</td></tr> } }).collect_view()}
                                    </table>
                                } })
                            }}
                        }.into_any()
                    }}
                </div>
                <div class="pane pad">
                    {let open_slowest = open_slowest.clone(); move || {
                        let m = list.get().into_iter().find(|m| s(m, "name") == name.get())?;
                        let b = breakdown.get();
                        let unit = s(&m, "unit");
                        let stat = s(&b, "stat");
                        let series = b["series"].as_array().cloned().unwrap_or_default();
                        let hosts: Vec<(String, bool)> = b["hosts"].as_array().map(|a| a.iter().map(|h| (s(h, "key"), h["stuntman"] == true)).collect()).unwrap_or_default();
                        let query_text = format!("query_metric(\"{}\", stat=\"{}\", by=\"{}\", since=\"{}\")", name.get(), stat, by.get(), sh.since.get());
                        let last = series.iter().map(|x| points(x).iter().filter(|p| p.1 > 0.0).map(|p| p.0).max().unwrap_or(0)).max().unwrap_or(0);
                        let open = open_slowest.clone();
                        Some(view! {
                            <h2 class="mono" style="font-weight:500;margin:0">{s(&m, "name")}</h2>
                            <div class="meta" style="margin-top:6px">{format!("{}  ·  {}  ·  from {}", match s(&m, "kind").as_str() { "histogram" => "Histogram", "counter" => "Counter", "gauge" => "Gauge", _ => "Up-down counter" }, if unit.is_empty() { "no unit".to_string() } else { unit.clone() }, m["services"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" and ")).unwrap_or_default())}</div>
                            <div class="row" style="margin-top:12px">
                                <button class="btn small" on:click=move |_| open(if last > 0 { last } else { api::now() as i64 })>"Open slowest trace ›"</button>
                                <button class="btn small" on:click=move |_| ui::copy(&query_text, "Query")>"Copy as query"</button>
                            </div>
                            <div class="row" style="margin-top:24px"><span class="caps grow">"Breakdown by"</span>
                                <select class="select" on:change=move |e| by.set(event_target_value(&e))>
                                    {{
                                        let mut opts: Vec<String> = BREAKDOWNS.iter().map(|x| x.to_string()).collect();
                                        for k in chart.get()["attributes"].as_array().cloned().unwrap_or_default() {
                                            let k = k.as_str().unwrap_or("").to_string();
                                            if !opts.contains(&k) { opts.insert(opts.len() - 1, k); }
                                        }
                                        opts.into_iter().map(|k| { let sel = by.get() == k; let label = if k.is_empty() { "No breakdown".to_string() } else { k.clone() }; view! { <option value=k selected=sel>{label}</option> } }).collect_view()
                                    }}
                                </select></div>
                            <table class="kv" style="margin-top:10px">
                                <tr><td style="color:var(--caption)"></td><td class="caps" style="width:80px">{stat.to_uppercase()}</td><td class="caps" style="width:70px">"Calls"</td><td class="caps" style="width:60px">"Errors"</td></tr>
                                {series.iter().take(20).map(|x| {
                                    let k = s(x, "key");
                                    let real = by.get() == "server.address" && hosts.iter().any(|h| h.0 == k && !h.1);
                                    let v = x["value"].as_f64();
                                    let errs = x["errors"].as_f64().unwrap_or(0.0);
                                    let has_err = errs > 0.0;
                                    view! { <tr><td class="mono" class:attn=real>{k.clone()}{real.then(|| view! { <div class="small attn" style="font-family:var(--sans)">"not Stuntman: this call went to the real provider"</div> })}</td>
                                        <td class="mono">{v.map(|v| fmt(v, &unit)).unwrap_or("–".into())}</td><td class="mono">{format!("{:.0}", x["count"].as_f64().unwrap_or(0.0))}</td>
                                        <td class="mono" class:danger=has_err>{format!("{errs:.0}")}</td></tr> }
                                }).collect_view()}
                            </table>
                            {move || {
                                let w = why.get();
                                let j = w["jump"].as_object().cloned()?;
                                let unit = unit.clone();
                                let d = w["drivers"].as_array().and_then(|a| a.first()).cloned().unwrap_or(Value::Null);
                                let (from, to) = (j["from"].as_i64().unwrap_or(0), j["to"].as_i64().unwrap_or(0));
                                let provoked: Vec<Value> = w["provoked"].as_array().cloned().unwrap_or_default();
                                let host = s(&d, "key");
                                let during = d["during"].as_f64().map(|v| fmt(v, &unit)).unwrap_or("–".into());
                                let before = d["before"].as_f64().map(|v| fmt(v, &unit)).unwrap_or("nothing".into());
                                let errs = d["errors"].as_f64().unwrap_or(0.0);
                                let calls = d["calls"].as_f64().unwrap_or(0.0);
                                let col = s(&w["collector"], "answer");
                                Some(view! {
                                    <div class="caps" style="margin-top:28px">"Why it jumped"</div>
                                    <div class="s12 attn" style="margin-top:8px">{format!("From {} to {} {} went from {} to {} on {}{}{}.", api::hm(from), api::hm(to), s(&w, "by"), before, during, host,
                                        if calls > 0.0 { format!(" ({calls:.0} calls, {errs:.0} errors)") } else { String::new() },
                                        if d["stuntman"] == true { "; that host is Stuntman, so the slowness or failures were answered by it".to_string() } else { "; that host is not Stuntman: the real provider was slow".to_string() })}</div>
                                    {(!provoked.is_empty()).then(|| {
                                        let rules = w["rules"].as_array().cloned().unwrap_or_default();
                                        view! {
                                            <div class="s12 attn" style="margin-top:6px">{rules.iter().map(|r| {
                                                let n = api::n(r, "count");
                                                let what = match (s(r, "module").as_str(), n) { ("email", 1) => "an e-mail".to_string(), ("email", n) => format!("{n} e-mails"), ("webhook", 1) => "a webhook".into(), ("webhook", n) => format!("{n} webhooks"), (_, n) => format!("{n} calls") };
                                                let status = s(r, "status");
                                                let verb = match status.as_str() { "bounced" => "bounce".to_string(), "dropped" => "be dropped".into(), "deferred" => "be deferred".into(), "reset" => "get a reset connection".into(), "empty" => "get an empty reply".into(), "no answer" => "get no answer".into(), c => format!("get {c}") };
                                                format!("The rule “{}” made {what} {verb}.", s(r, "name"))
                                            }).collect::<Vec<_>>().join(" ")}</div>
                                            <div class="row" style="margin-top:8px;gap:20px">{rules.iter().map(|r| view! { <a class="link" href=s(r, "link")>"Open the rule ›"</a> }).collect_view()}</div>
                                        }
                                    })}
                                    {(col != "normal" && !col.is_empty()).then(|| view! { <div class="s12 attn" style="margin-top:6px">{format!("The telemetry collector is provoked too ({col}).")}</div> })}
                                    {(d["stuntman"] == false).then(|| view! { <a class="link" href="/services/email">"Stand in for this host ›"</a> })}
                                })
                            }}
                            {(series.len() > 20).then(|| view! { <div class="small attn" style="margin-top:8px">{format!("{} has {} values here. Showing the 20 biggest. A real APM bills per series, so this would cost money in production; usually the attribute should be dropped.", by.get(), series.len())}</div> })}
                            {(api::now() as i64 - api::n(&m, "last") > 5 * 60_000).then(|| view! { <div class="small attn" style="margin-top:12px">{format!("Nothing since {}: the app stopped sending this metric. If its traces stopped too, the service may be down.", api::hm(api::n(&m, "last")))}</div> })}
                        })
                    }}
                </div>
            </div>
        </div>
    }
}
