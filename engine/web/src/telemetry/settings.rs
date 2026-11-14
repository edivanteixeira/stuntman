//! T3 · Telemetry › Exporter settings: receive, signals, provoke the collector, forward to a real
//! APM, storage. The form edits telemetry/<env>.yaml, shown beside it.

use leptos::prelude::*;
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui::{self, Confirm, Toggle},
};

#[component]
pub fn Settings() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let info = RwSignal::new(Value::Null);
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let clear = RwSignal::new(false);
    let changed_elsewhere = RwSignal::new(false);
    let elsewhere_text = RwSignal::new(String::new());

    let load = move |keep: bool| {
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/telemetry/settings").await {
                let dirty = draft.get_untracked() != saved.get_untracked();
                if keep && dirty && v["settings"] != saved.get_untracked() {
                    // Say who changed what, and what the person's own pending change is.
                    let (old, new) = (saved.get_untracked(), v["settings"].clone());
                    let mine = draft.get_untracked();
                    let paths = [("/signals/traces/on", "traces on/off"), ("/signals/logs/on", "logs on/off"), ("/signals/metrics/on", "metrics on/off"), ("/signals/traces/keep", "traces keep"),
                        ("/signals/logs/keep", "logs keep"), ("/signals/metrics/keep", "metrics keep"), ("/forward/enabled", "forwarding"), ("/forward/endpoint", "the endpoint"), ("/receive/anyApiKey", "any api-key")];
                    let theirs: Vec<String> = paths.iter().filter(|(p, _)| old.pointer(p) != new.pointer(p)).map(|(p, l)| format!("{l} → {}", new.pointer(p).cloned().unwrap_or_default())).collect();
                    let yours: Vec<String> = paths.iter().filter(|(p, _)| old.pointer(p) != mine.pointer(p)).map(|(p, l)| format!("{l} → {}", mine.pointer(p).cloned().unwrap_or_default())).collect();
                    let who = v["lastChange"].clone();
                    elsewhere_text.set(format!("{} changed while you were editing{}. It set {}. Your change: {}.",
                        s(&v, "file"), if who.is_null() { " (the file was edited)".to_string() } else { format!("  ·  by {} at {}", s(&who, "by").replace("mcp:", "").replace("api:", "") + if s(&who, "by").starts_with("mcp:") { " via MCP" } else { "" }, api::hm(api::n(&who, "at"))) },
                        if theirs.is_empty() { "something else".into() } else { theirs.join(", ") }, if yours.is_empty() { "none".into() } else { yours.join(", ") }));
                    changed_elsewhere.set(true);
                } else if !(keep && dirty) {
                    draft.set(v["settings"].clone());
                }
                saved.set(v["settings"].clone());
                info.set(v);
            }
        });
    };
    load(false);
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "telemetry.settings" {
            load(true);
        }
        t
    });
    let set = move |path: &'static str, v: Value| draft.update(|d| if let Some(x) = d.pointer_mut(path) { *x = v; });
    let val = move |path: &'static str| draft.get().pointer(path).cloned().unwrap_or(Value::Null);
    let dirty = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        ["/receive/anyApiKey", "/signals/traces", "/signals/logs", "/signals/metrics", "/forward/enabled", "/forward/endpoint", "/storage/max"].iter().filter(|p| a.pointer(p) != b.pointer(p)).count()
    });
    let endpoint_bad = move || {
        let e = s(&val("/forward"), "endpoint");
        (val("/forward/enabled") == true || !e.is_empty()) && !((e.starts_with("https://") || e.starts_with("http://")) && e.split("://").nth(1).map(|h| h.contains(':')).unwrap_or(false))
    };
    let save = move |_| {
        let body = json!({"settings": draft.get_untracked()});
        leptos::task::spawn_local(async move {
            match api::send("PUT", "/telemetry/settings", body).await {
                Ok(v) => { saved.set(v["settings"].clone()); draft.set(v["settings"].clone()); info.set(v); err.set(String::new()); changed_elsewhere.set(false); ui::toast("Saved"); }
                Err(e) => err.set(e),
            }
        });
    };
    // Provoking acts at once (it is an action, not a setting): it starts now and ends by itself.
    let provoke = move |answer: String, only: String| {
        leptos::task::spawn_local(async move {
            let r = if answer == "normal" { api::send("DELETE", "/telemetry/collector/provoke", json!({})).await }
                    else { api::send("POST", "/telemetry/collector/provoke", json!({"answer": answer, "onlyFor": only, "for": "10m"})).await };
            match r {
                Ok(_) => load(true),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let do_clear = move |_| {
        leptos::task::spawn_local(async move {
            match api::send("DELETE", "/telemetry/data", json!({"confirm": true})).await {
                Ok(v) => { ui::toast(format!("Cleared {} traces, {} log lines, {} series", v["traces"], v["logs"], v["series"])); load(true); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let envname = move || api::s(&env.get(), "env").to_uppercase();
    let keep_opts = |v: Value| [("1h", "1 h"), ("6h", "6 h"), ("24h", "24 h"), ("2d", "2 days"), ("7d", "7 days")].into_iter().map(move |(k, l)| view! { <option value=k selected=v == json!(k)>{l}</option> }).collect_view();

    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <h1><span class="crumb">"Telemetry"</span><span class="crumb" style="margin:0 12px">"/"</span>"Exporter settings"</h1>
                    <span class="sub">{move || format!("OpenTelemetry in {}", envname())}</span><span class="grow"></span>
                    {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                        <button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> }) }}
                    <button class="btn primary" disabled=move || dirty.get() == 0 || endpoint_bad() on:click=save>"Save"</button>
                </div>
                <div class="tabs"><a class="link" style="align-self:center" href="/telemetry/traces">"‹ Traces and logs"</a></div>
            </div>
            {move || changed_elsewhere.get().then(|| view! {
                <div class="strip">{move || elsewhere_text.get()}
                    <button class="link" on:click=move |_| { draft.set(saved.get_untracked()); changed_elsewhere.set(false); }>"Load the new file"</button>
                    <button class="link" on:click=move |_| changed_elsewhere.set(false)>"Keep mine and save over it"</button></div>
            })}
            <div class="body">
                <div class="pane pad form" style="max-width:780px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let i = info.get();
                        let ep = i["endpoints"].clone();
                        let (h, g) = (s(&ep, "http"), s(&ep, "grpc"));
                        let (h2, g2) = (h.clone(), g.clone());
                        let last = i["last15min"].clone();
                        let c = d["collector"].clone();
                        let provoked = s(&c, "answer") != "normal";
                        let used = api::n(&i, "usedBytes");
                        let max = api::n(&i, "maxBytes").max(1);
                        let fwd = i["lastForward"].clone();
                        let near_full = used * 10 > max * 9;
                        view! {
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Receive"</div>
                                <div class="row" style="margin-bottom:8px"><span style="width:160px">"OTLP/HTTP"</span><span class="mono grow">{h}</span><button class="link" on:click=move |_| ui::copy(&h2, "Endpoint")>"Copy"</button></div>
                                <div class="row" style="margin-bottom:12px"><span style="width:160px">"OTLP/gRPC"</span><span class="mono grow">{g}</span><button class="link" on:click=move |_| ui::copy(&g2, "Endpoint")>"Copy"</button></div>
                                <div class="row"><Toggle on=Signal::derive(move || val("/receive/anyApiKey") == true) set=move |v: bool| set("/receive/anyApiKey", json!(v))/>
                                    <span>"Accept any api-key header"</span><span class="small t2">"so the New Relic or Datadog setting in the app can stay"</span></div>
                            </div>
                            <div class="block">
                                <div class="row caps" style="margin-bottom:10px"><span class="grow">"Signals"</span><span style="width:140px">"Keep"</span><span style="width:160px">"Last 15 min"</span></div>
                                {[("traces", "Traces", "/signals/traces/on", "/signals/traces/keep", format!("{} traces", api::n(&last, "traces"))),
                                  ("logs", "Logs", "/signals/logs/on", "/signals/logs/keep", format!("{} lines", api::n(&last, "logs"))),
                                  ("metrics", "Metrics", "/signals/metrics/on", "/signals/metrics/keep", format!("{} series", api::n(&last, "series")))].into_iter().map(|(_, l, on_p, keep_p, n)| view! {
                                    <div class="row" style="margin-bottom:10px">
                                        <Toggle on=Signal::derive(move || val(on_p) == true) set=move |v: bool| set(on_p, json!(v))/>
                                        <span class="grow">{l}</span>
                                        <select class="select" style="width:140px" class:changed=move || saved.get().pointer(keep_p) != draft.get().pointer(keep_p) on:change=move |e| set(keep_p, json!(event_target_value(&e)))>{keep_opts(val(keep_p))}</select>
                                        <span class="mono small t2" style="width:160px">{n}</span>
                                    </div>
                                }).collect_view()}
                                <div class="small t2">"A signal turned off is answered 200 and dropped, so the app never retries or blocks."</div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Provoke the collector"</div>
                                <div class="row" style="margin-bottom:10px"><span style="width:160px">"Answer"</span>
                                    <select class="select" on:change=move |e| provoke(event_target_value(&e), s(&draft.get_untracked()["collector"], "onlyFor"))>
                                        {i["answers"].as_array().cloned().unwrap_or_default().into_iter().map(|a| { let k = a[0].as_str().unwrap_or("").to_string(); let l = a[1].as_str().unwrap_or("").to_string(); let sel = s(&c, "answer") == k; view! { <option value=k selected=sel>{l}</option> } }).collect_view()}
                                    </select>
                                    <span class="small t2">"403, down or slow: the app must keep serving its own requests."</span></div>
                                <div class="row"><span style="width:160px">"Only for"</span>
                                    <select class="select" on:change=move |e| { let only = event_target_value(&e); let a = s(&draft.get_untracked()["collector"], "answer"); set("/collector/onlyFor", json!(only.clone())); if a != "normal" { provoke(a, only) } }>
                                        <option value="all" selected=s(&c, "onlyFor") == "all">"All services"</option>
                                        {last["services"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, _)| { let sel = s(&c, "onlyFor") == k; view! { <option value=k.clone() selected=sel>{k.clone()}</option> } }).collect_view()}
                                    </select>
                                    <span class="small t2">"Shown as a strip on Traces and Logs while it is on, with Stop. Ends by itself after 10 min."</span></div>
                                {provoked.then(|| view! { <div class="small provoked" style="margin-top:8px">{format!("on since {} by {}  ·  ends at {}", api::hm(api::n(&c, "since")), s(&c, "setBy").trim_start_matches("api:").trim_start_matches("mcp:"), api::hm(api::n(&c, "until")))}</div> })}
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Forward to a real APM"</div>
                                <div class="row" style="margin-bottom:12px"><Toggle on=Signal::derive(move || val("/forward/enabled") == true) set=move |v: bool| set("/forward/enabled", json!(v))/><span>"Also send everything to another OTLP endpoint"</span></div>
                                <div class="row" style="margin-bottom:6px"><span style="width:160px" class:cap=val("/forward/enabled") != true>"Endpoint"</span>
                                    <input class="input mono grow" class:bad=endpoint_bad placeholder="https://otlp.example-apm.com:4318" prop:value=s(&d["forward"], "endpoint") on:input=move |e| set("/forward/endpoint", json!(event_target_value(&e)))/></div>
                                {move || endpoint_bad().then(|| view! { <div class="err" style="margin-left:160px">"Start with https:// and include the port, for example https://otlp.example-apm.com:4318. Save stays off until the endpoint is valid."</div> })}
                                {d["forward"]["headers"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| view! {
                                    <div class="row" style="margin:8px 0"><span style="width:160px" class="cap">"Header"</span><span class="mono t2">{k}</span>
                                        <span class="link" style="font-size:13px">{match v.as_str().unwrap_or("").strip_prefix("secret://") { Some(n) => format!("from Secrets › {n}"), None => "a reference (env:NAME)".to_string() }}</span></div>
                                }).collect_view()}
                                {(!fwd.is_null() && fwd["ok"] == false).then(|| view! { <div class="small danger">{format!("Last forward failed at {}: {}", api::hm(api::n(&fwd, "at")), if fwd["status"].is_null() { s(&fwd, "error") } else { format!("{} from {}", fwd["status"], s(&fwd, "host")) })}</div> })}
                                {(!fwd.is_null() && fwd["ok"] == true).then(|| view! { <div class="small ok">{format!("Last forward {}: {} OK", api::hm(api::n(&fwd, "at")), fwd["status"])}</div> })}
                                <div class="small t2" style="margin-top:6px">"Off by default, so staging data stays here. When on, a failed forward never fails the app."</div>
                            </div>
                            <div class="block row">
                                <span class="caps" style="width:160px">"Storage"</span>
                                <span class="grow" class:attn=near_full>{format!("{} of {} used  ·  the oldest data is dropped first", api::kb(used), s(&d["storage"], "max"))}</span>
                                <button class="link danger" style="font-size:13px" on:click=move |_| clear.set(true)>"Clear all telemetry"</button>
                            </div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                            <div class="small t2">{format!("Fields come from {}. The same file is the source of truth.", s(&i, "file"))}</div>
                        }.into_any()
                    }}
                </div>
                <div class="side yaml" style="width:420px">
                    <div class="row"><span class="caps">{move || s(&info.get(), "file")}</span><span class="small ok">"live"</span></div>
                    <pre class="yaml" style="background:var(--surface);border:1px solid var(--rule);border-radius:6px;padding:12px">{move || s(&info.get(), "yaml")}</pre>
                    {move || (dirty.get() > 0).then(|| view! { <div class="small attn">"Changed, not saved yet: the file updates on Save. Editing the file by hand updates this form."</div> })}
                </div>
            </div>
            <Confirm open=clear question=Signal::derive(move || format!("Clear all telemetry in {}?", envname()))
                detail=Signal::derive(move || { let l = info.get()["last15min"].clone(); format!("Traces, log lines and metric series are deleted ({} traces, {} lines in the last 15 min). Settings and the YAML file stay. This cannot be undone.", api::n(&l, "traces"), api::n(&l, "logs")) })
                action="Clear telemetry" on_yes=do_clear/>
        </div>
    }
}
