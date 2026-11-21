//! A1 · APIs › Calls: services (plugins) on the left, every call as a sentence in the middle, the
//! call on the right: what the app sent, what Stuntman answered, next calls answer, raw HTTP, billing.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use serde_json::{json, Value};

use super::{answer_class, services, PluginHead, RangeSearch};
use crate::{
    api::{self, enc, s},
    ui::{self, Pop},
};

#[component]
pub fn Calls() -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let pm = use_params_map();
    // The range lives in the rail (the area), so it survives a trip to another tab or service.
    let since = use_context::<super::Range>().map(|r| r.0).unwrap_or_else(|| RwSignal::new("15m".to_string()));
    // ?since= and ?q= open that view (a link someone sent); changing them writes them back.
    if let Some(x) = qm.get_untracked().get("since").filter(|x| ["5m", "15m", "1h", "1d"].contains(&x.as_str())) { since.set(x); }
    let q = RwSignal::new(qm.get_untracked().get("q").unwrap_or_default());
    let tail = move |call: &str| {
        let mut v = vec![];
        if !call.is_empty() { v.push(format!("call={}", enc(call))); }
        if since.get_untracked() != "15m" { v.push(format!("since={}", since.get_untracked())); }
        if !q.get_untracked().is_empty() { v.push(format!("q={}", enc(&q.get_untracked()))); }
        if v.is_empty() { String::new() } else { format!("?{}", v.join("&")) }
    };
    let list = services();
    let calls = RwSignal::new(Vec::<Value>::new());
    let call = RwSignal::new(Value::Null);
    let raw = RwSignal::new(false);
    let svc = Memo::new(move |_| pm.get().get("service").or_else(|| qm.get().get("service")).unwrap_or_else(|| super::area::pick(&list.get())));
    let sel = Memo::new(move |_| qm.get().get("call").unwrap_or_default());
    let load = move || {
        let (sv, si, qq) = (svc.get_untracked(), since.get_untracked(), q.get_untracked());
        leptos::task::spawn_local(async move {
            let path = format!("/apis/calls?since={si}{}{}", if sv.is_empty() { String::new() } else { format!("&service={}", enc(&sv)) }, if qq.is_empty() { String::new() } else { format!("&q={}", enc(&qq)) });
            if let Ok(v) = api::get(&path).await {
                calls.set(v["calls"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    let load_call = move || {
        let id = sel.get_untracked();
        if id.is_empty() { call.set(Value::Null); return; }
        leptos::task::spawn_local(async move { call.set(api::get(&format!("/apis/calls/{}?raw=true", enc(&id))).await.unwrap_or(Value::Null)); });
    };
    Effect::new(move |_| { svc.track(); since.track(); q.track(); load(); });
    let nav_url = nav.clone();
    Effect::new(move |prev: Option<()>| {
        since.track(); q.track();
        if prev.is_none() { return; }
        let sv = svc.get_untracked();
        if sv.is_empty() { return; }
        nav_url(&format!("/apis/{}/calls{}", enc(&sv), tail(&sel.get_untracked())), leptos_router::NavigateOptions { replace: true, scroll: false, ..Default::default() });
    });
    Effect::new(move |_| { sel.track(); raw.set(false); load_call(); });
    // New calls appear within half a second, without one fetch per call under load.
    let calm = api::calm();
    Effect::new(move |prev: Option<()>| {
        calm.track();
        if prev.is_some() { load(); }
    });
    let cur = Memo::new(move |_| list.get().into_iter().find(|x| s(x, "name") == svc.get()).unwrap_or(Value::Null));
    let nav2 = nav;
    view! {
        <div class="page">
            {move || view! { <PluginHead service=svc.get() active="calls" right=move || view! { <RangeSearch since=since q=q/> }/> }}
            {move || {
                let c = cur.get();
                let f = c["forced"].clone();
                view! {
                    {(api::n(&c, "billableToday") > 0 && c["overWarn"] == true).then(|| view! {
                        <div class="strip">{format!("{} billable {} calls today, above {}. Usually a retry loop.", api::n(&c, "billableToday"), s(&c, "title"), api::n(&c, "warnAbove"))}</div> })}
                    {(!f.is_null()).then(|| { let n = s(&c, "name"); view! {
                        <div class="strip">{format!("{} for {}  ·  by {}", s(&f, "answer").replace('_', " "), if f["left"].is_number() { format!("the next {} call(s)", api::n(&f, "left")) } else { format!("until {}", api::hm(api::n(&f, "until"))) }, s(&f, "by").trim_start_matches("api:").trim_start_matches("mcp:"))}
                            <button class="link" on:click=move |_| { let n = n.clone(); leptos::task::spawn_local(async move { let _ = api::send("POST", &format!("/apis/{}/force", enc(&n)), json!({"answer": "default"})).await; }) }>"Stop"</button></div> } })}
                }
            }}
            <div class="body">
                <div class="pane files">
                    {move || {
                        let c = cur.get();
                        (!c.is_null()).then(|| view! {
                            <div class="crumbs row"><span class="strong" style="font-size:15px">{move || match since.get().as_str() { "1h" => "Last hour", "1d" => "Last 24 hours", "5m" => "Last 5 minutes", _ => "Last 15 minutes" }}</span>
                                <span class="small attn" style="margin-left:14px">{format!("{} today{}", api::thousands(api::n(&c, "callsToday")),
                                    if c["billable"] == true { format!("  ·  {} billable  ·  about ${:.2} at the price you set", api::n(&c, "billableToday"), c["costToday"].as_f64().unwrap_or(0.0)) } else { String::new() })}</span></div>
                        })
                    }}
                    <div class="ftable">
                        {move || {
                            let nav = nav2.clone();
                            calls.get().into_iter().map(|c| {
                                let id = s(&c, "id");
                                let (i1, sv) = (id.clone(), s(&c, "service"));
                                let cls = answer_class(api::n(&c, "status"), &s(&c, "answer"));
                                let nav = nav.clone();
                                view! { <div class="frow" class:on=move || sel.get() == i1 on:click=move |_| nav(&format!("/apis/{}/calls{}", enc(&sv), tail(&id)), leptos_router::NavigateOptions { scroll: false, ..Default::default() })>
                                    <span class="mono small cap" style="width:72px">{api::hms(api::n(&c, "at"))}</span>
                                    <div class="grow"><div>{s(&c, "sentence")}</div>
                                        <div class=format!("small {cls}")>{format!("{}{}", s(&c, "answer").replace('_', " "), if s(&c, "provoked").is_empty() { String::new() } else { format!("  ·  {}", s(&c, "provoked")) })}</div></div></div> }
                            }).collect_view()
                        }}
                    </div>
                    {move || (calls.get().is_empty()).then(|| view! {
                        <div class="callout" style="margin:20px 24px"><div class="t">{move || { let today = api::n(&cur.get(), "callsToday"); if !q.get().is_empty() { format!("No calls match “{}”", q.get()) } else if today > 0 && since.get() != "1d" { format!("No calls in this range  ·  {} today", api::thousands(today)) } else { format!("No API calls in {}", s(&env.get(), "env").to_uppercase()) } }}</div>
                            {move || (api::n(&cur.get(), "callsToday") > 0 && since.get() != "1d").then(|| view! { <button class="link" style="margin-bottom:8px" on:click=move |_| since.set("1d".into())>"Show the last 24 h ›"</button> })}
                            <div class="t2">{move || { let c = cur.get(); if !q.get().is_empty() || (api::n(&c, "callsToday") > 0 && since.get() != "1d") { String::new() } else if c.is_null() { "Install a plugin from the catalog, then point the app's base URL at Stuntman.".to_string() } else { format!("Point the app's base URL at {} (or the real host {} with the Stuntman Test CA). Auth and limits shows it.", s(&c["urls"], "local"), if s(&c["urls"], "real").is_empty() { "—".into() } else { s(&c["urls"], "real") }) } }}</div>
                            {move || { let c = cur.get(); if !q.get().is_empty() || (api::n(&c, "callsToday") > 0 && since.get() != "1d") { ().into_any() } else if c.is_null() { view! { <a class="link" href="/apis/new">"+ New service"</a> }.into_any() } else { view! { <a class="link" href=format!("/apis/{}/auth", enc(&s(&c, "name")))>"Auth & limits ›"</a> }.into_any() } }}</div> })}
                    <div class="small cap" style="padding:14px 24px">"Sentences come from the plugin; Raw HTTP is one click away on the right."</div>
                </div>
                <div class="side envside">
                    {move || {
                        let c = call.get();
                        if c.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick a call to see what the app sent and what Stuntman answered."</div> }.into_any(); }
                        detail(c, cur.get(), raw).into_any()
                    }}
                </div>
            </div>
        </div>
    }
}

fn kv_rows(v: &Value) -> impl IntoView {
    // Long values (hashes, base64, nested JSON) get the full width under their name, cut at 240
    // characters; the whole body is in Raw HTTP.
    v.as_object().cloned().unwrap_or_default().into_iter().map(|(k, x)| {
        let text = match x { Value::String(s) => s, o => o.to_string() };
        if text.chars().count() <= 40 {
            return view! { <tr><td>{k}</td><td class="mono">{text}</td></tr> }.into_any();
        }
        let n = text.chars().count();
        let shown: String = text.chars().take(240).collect();
        view! {
            <tr><td colspan="2" style="padding-bottom:0">{k}</td></tr>
            <tr><td colspan="2" class="mono kv-long">{shown}{(n > 240).then(|| view! { <span class="cap">{format!("  … {} more characters (Raw HTTP)", n - 240)}</span> })}</td></tr>
        }.into_any()
    }).collect_view()
}

fn detail(c: Value, svc: Value, raw: RwSignal<bool>) -> impl IntoView {
    let next = RwSignal::new(false);
    let sv = s(&c, "service");
    let curl = {
        let r = &c["request"];
        let headers: String = r["headers"].as_object().cloned().unwrap_or_default().into_iter().filter(|(k, _)| !["host", "content-length", "connection"].contains(&k.as_str())).map(|(k, v)| format!(" -H '{}: {}'", k, v.as_str().unwrap_or(""))).collect();
        let body = s(r, "body");
        format!("curl -X {} 'http://127.0.0.1:8028/{}{}'{headers}{}", s(&c, "method"), sv, s(&c, "path"), if body.is_empty() { String::new() } else { format!(" --data '{}'", body.replace('\'', "'\\''")) })
    };
    let answers = RwSignal::new(Vec::<String>::new());
    let sv2 = sv.clone();
    leptos::task::spawn_local(async move {
        if let Ok(v) = api::get(&format!("/plugins/{}/answers", enc(&sv2))).await {
            answers.set(v["answers"].as_array().cloned().unwrap_or_default().iter().map(|a| s(&a["answer"], "name")).collect());
        }
    });
    let force = move |a: String| {
        let sv = sv.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/apis/{}/force", enc(&sv)), json!({"answer": a, "count": 1})).await {
                Ok(_) => ui::toast(if a == "default" { "Back to the data".to_string() } else { format!("The next call answers {}", a.replace('_', " ")) }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let forced = svc["forced"].clone();
    let route = s(&c, "route");
    let editor = format!("/apis/{}/routes?route={}", enc(&s(&c, "service")), enc(&route));
    let answers_href = format!("/apis/{}/answers", enc(&s(&c, "service")));
    let trace = s(&c, "traceId");
    let req = c["request"].clone();
    let resp = c["response"].clone();
    view! {
        <h2 class="h2">{s(&c, "sentence")}</h2>
        <div class="small cap">{format!("{} {}  ·  {} in {} ms", s(&c, "method"), s(&c, "path"), api::n(&c, "status"), api::n(&c, "ms"))}</div>
        <div class="row" style="margin:12px 0;gap:8px">
            <button class="btn" class:armed=move || raw.get() on:click=move |_| raw.update(|r| *r = !*r)>"Raw HTTP"</button>
            <button class="btn" on:click=move |_| ui::copy(&curl, "curl")>"Copy as curl"</button>
            {(!trace.is_empty()).then(|| view! { <a class="link" href=format!("/telemetry/traces/{trace}")>{format!("trace {} ↗", &trace[..8.min(trace.len())])}</a> })}
        </div>
        {move || raw.get().then(|| view! {
            <div class="caps" style="margin:10px 0 6px">"Request"</div>
            <pre class="yaml envyaml" style="max-height:220px;overflow:auto">{format!("{} {}\n{}\n\n{}", s(&req, "method"), s(&req, "path"),
                req["headers"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n"), s(&req, "body"))}</pre>
            <div class="caps" style="margin:10px 0 6px">"Response"</div>
            <pre class="yaml envyaml" style="max-height:220px;overflow:auto">{format!("{}\n{}\n\n{}", api::n(&resp, "status"),
                resp["headers"].as_array().cloned().unwrap_or_default().iter().map(|h| format!("{}: {}", h[0].as_str().unwrap_or(""), h[1].as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n"), s(&resp, "body"))}</pre>
            {(api::n(&req, "size") > 64 * 1024 || api::n(&resp, "size") > 64 * 1024).then(|| view! { <div class="small cap">"Bodies over 64 KB are cut here."</div> })}
            {(!s(&resp, "encode").is_empty()).then(|| view! { <div class="small cap">{format!("The response body is shown as text; it was sent as {} (raw bytes, no Content-Encoding).", s(&resp, "encode"))}</div> })}
        })}
        <div class="caps" style="margin:18px 0 8px">"The app sent"</div>
        <table class="kv2">{kv_rows(&c["sent"])}{c["sent"].as_object().map(|o| o.is_empty()).unwrap_or(true).then(|| view! { <tr><td class="cap">"(no declared fields)"</td><td></td></tr> })}</table>
        {(s(&c, "answer") == "recorded").then(|| view! {
            <div class="caps" style="margin:18px 0 8px">"Recorded"</div>
            <div class="small" class:danger=c["steps"][0]["failed"] == true>{s(&c["steps"][0], "text")}</div>
            <div class="small cap" style="margin-top:4px">"The app got the provider's real answer; what is kept here has secrets, card numbers, CPFs and e-mails masked. Raw HTTP shows it."</div>
        })}
        <div class="caps" style="margin:18px 0 8px" class:hidden=s(&c, "answer") == "recorded">"Stuntman answered"</div>
        <table class="kv2" class:hidden=s(&c, "answer") == "recorded">{kv_rows(&c["answered"])}<tr><td>"answer"</td><td class="mono">{format!("{}{}", s(&c, "answer"), if s(&c, "provoked").is_empty() { "  ·  by the data".to_string() } else { format!("  ·  {}", s(&c, "provoked")) })}</td></tr></table>
        <div class="small cap" style="margin-top:6px">{c["steps"].as_array().cloned().unwrap_or_default().iter().filter(|x| x["step"] == "decide").map(|x| format!("Decided: {}", s(x, "text"))).collect::<Vec<_>>().join(" · ")}</div>
        {(!c["sends"].as_array().map(|a| a.is_empty()).unwrap_or(true)).then(|| view! {
            <div class="caps" style="margin:18px 0 8px">"Webhooks sent after"</div>
            {c["sends"].as_array().cloned().unwrap_or_default().into_iter().map(|w| view! { <div class="hrow"><span class="mono small cap">{api::hms(api::n(&w, "at"))}</span><div><div class="mono small">{s(&w, "url")}</div>
                <div class="small" class:ok=w["status"].as_i64().map(|x| x < 300).unwrap_or(false) class:danger=w["status"].is_null()>{if w["status"].is_null() { s(&w, "error") } else { format!("{} in {} ms", w["status"], api::n(&w, "ms")) }}</div></div></div> }).collect_view()}
        })}
        <div class="caps" style="margin:22px 0 8px">"Next calls answer"</div>
        <span class="anchor"><button class="btn" class:armed=!forced.is_null() on:click=move |_| next.set(true)>{if forced.is_null() { "By the data (default)".to_string() } else { s(&forced, "answer").replace('_', " ") }}<span class="small">" ▾"</span></button>
            <Pop open=next>
                <div class="mi" on:click={ let f = force.clone(); move |_| f("default".into()) }>"By the data (default)"</div>
                {let force = force.clone(); move || answers.get().into_iter().map(|a| { let f = force.clone(); let a2 = a.clone(); view! { <div class="mi" on:click=move |_| f(a2.clone())>{a.replace('_', " ")}</div> } }).collect_view()}
                <div class="rule"></div>
                {[("reset", "Connection reset"), ("empty", "Empty reply"), ("timeout", "Timeout"), ("401", "401"), ("403", "403"), ("500", "500"), ("503", "503"), ("html", "HTML page with 200")].into_iter().map(|(v, l)| { let f = force.clone(); view! { <div class="mi" on:click=move |_| f(v.into())>{l}</div> } }).collect_view()}
            </Pop></span>
        <div class="small cap" style="margin-top:6px">"Forces the next call; each choice is a named answer of the plugin or a generic failure."</div>
        <div class="row" style="margin-top:14px;gap:20px"><a class="link" href=answers_href>"Answers ›"</a><a class="link" href=editor>"Open the route in the editor ›"</a></div>
        {(c["billable"] == true).then(|| view! { <div class="caps" style="margin:22px 0 8px">"Billing"</div>
            <div class="small t2">{format!("Billable: yes. {} bills every call, even failed ones; the app must not retry blindly.", s(&svc, "title"))}</div> })}
    }
}
