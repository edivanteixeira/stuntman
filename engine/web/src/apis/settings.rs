//! Settings tab of an HTTP API service: the plugin file (YAML, editable, validated as you type),
//! the data sets its answers read, where it came from, and removing the service.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm},
};

#[component]
pub fn Settings() -> impl IntoView {
    let p = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let plugin = RwSignal::new(Value::Null);
    let text = RwSignal::new(String::new());
    let errs = RwSignal::new(Vec::<Value>::new());
    let dirty = RwSignal::new(false);
    let remove = RwSignal::new(false);
    let load = move || {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/plugins/{}", enc(&sv))).await {
                if !dirty.get_untracked() { text.set(s(&v, "yaml")); }
                plugin.set(v);
            }
        });
    };
    Effect::new(move |_| { service.track(); dirty.set(false); load(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "apis.plugin" { load(); }
        t
    });
    let check = move |t: String| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::send("POST", &format!("/plugins/{}/validate", enc(&sv)), json!({"yaml": t})).await {
                errs.set(v["errors"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    let save = move || {
        let (sv, t, v) = (service.get_untracked(), text.get_untracked(), plugin.get_untracked()["version"].clone());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}", enc(&sv)), json!({"yaml": t, "version": v})).await {
                Ok(_) => { dirty.set(false); ui::toast("YAML saved"); load(); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let do_remove = move |_| {
        let sv = service.get_untracked();
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/plugins/{}", enc(&sv)), json!({})).await {
                Ok(_) => { ui::toast("Service removed"); nav("/apis", Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="settings" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"Unsaved YAML"</span>
                    <button class="btn" on:click=move |_| { dirty.set(false); errs.set(vec![]); text.set(s(&plugin.get_untracked(), "yaml")); }>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() || !errs.get().is_empty() on:click=move |_| save()>"Save YAML"</button>
            }/> }}
            <div class="body">
                <div class="pane pad">
                    <div class="row"><span class="caps grow">{move || format!("Plugin file  ·  {}", s(&plugin.get(), "file"))}</span>
                        <a class="link" href=move || format!("/api/plugins/{}/yaml", enc(&service.get())) download="">"Download"</a></div>
                    <textarea class="yaml" style="min-height:70vh;margin-top:10px;width:100%" prop:value=move || text.get()
                        on:input=move |e| { let v = event_target_value(&e); text.set(v.clone()); dirty.set(true); check(v); }></textarea>
                    {move || errs.get().into_iter().map(|e| view! { <div class="small danger">{format!("{}{}", e["line"].as_i64().map(|l| format!("line {l}: ")).unwrap_or_default(), s(&e, "error"))}</div> }).collect_view()}
                    <div class="small cap" style="margin-top:8px">"Routes, Answers and Auth & limits edit this same file; comments are lost when those tabs save."</div>
                </div>
                <div class="side envside">
                    {move || {
                        let v = plugin.get();
                        let pl = v["plugin"].clone();
                        let sv = pl["service"].clone();
                        let answers = format!("/apis/{}/answers", enc(&service.get()));
                        view! {
                            <div class="caps" style="margin:4px 0 8px">"Where it came from"</div>
                            <table class="kv2">
                                <tr><td>"origin"</td><td class="mono">{s(&sv, "origin")}</td></tr>
                                <tr><td>"region"</td><td class="mono">{if s(&sv, "region").is_empty() { "—".to_string() } else { s(&sv, "region") }}</td></tr>
                                <tr><td>"real host"</td><td class="mono">{if s(&sv, "real_host").is_empty() { "—".to_string() } else { s(&sv, "real_host") }}</td></tr>
                                <tr><td>"routes"</td><td class="mono">{pl["routes"].as_array().map(|a| a.len()).unwrap_or(0)}</td></tr>
                            </table>
                            <div class="caps" style="margin:22px 0 8px">"Data the answers read"</div>
                            <table class="kv2">
                                {pl["data"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, d)| view! {
                                    <tr><td>{k}</td><td class="mono">{format!("{} rows", d.as_array().map(|a| a.len()).unwrap_or(0))}</td></tr> }).collect_view()}
                            </table>
                            {pl["data"].as_object().map(|o| o.is_empty()).unwrap_or(true).then(|| view! { <div class="small cap">"No data sets: answers come from the request alone."</div> })}
                            <a class="link" style="display:inline-block;margin-top:8px" href=answers>"Edit the data in Answers ›"</a>
                            <SigningClock service=service.get() plugin=pl.clone()/>
                            <div class="caps" style="margin:28px 0 8px">"Remove"</div>
                            <div class="small t2">"Calls to this service become Not answered. The plugin file is deleted from this environment."</div>
                            <button class="btn danger" style="margin-top:10px" on:click=move |_| remove.set(true)>"Remove this service"</button>
                        }
                    }}
                </div>
            </div>
            <Confirm open=remove question=Signal::derive(move || format!("Remove {}?", service.get())) detail=Signal::derive(|| "Calls to it become Not answered; the plugin file is deleted from this environment.".to_string())
                action="Remove" on_yes=Callback::new(do_remove)/>
        </div>
    }
}

/// A10c · how this service signs its webhooks, Idempotency-Key replay, and the environment's clock.
#[component]
fn SigningClock(service: String, plugin: Value) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let wh = plugin["service"]["webhooks"].clone();
    let idem = plugin["service"]["idempotency"].is_object();
    let clock = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/clock").await { clock.set(v) } });
    });
    let sv = service.clone();
    let set = move |body: Value| { let sv = sv.clone(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/plugins/{}/service", enc(&sv)), body).await { Ok(_) => ui::toast("Saved"), Err(e) => ui::toast_err(e) } }) };
    let advance = move |by: &'static str| leptos::task::spawn_local(async move {
        let body = if by == "reset" { json!({"reset": true}) } else { json!({"by": by}) };
        match api::send("POST", "/clock/advance", body).await { Ok(v) => ui::toast(format!("The clock is at {}", s(&v, "now"))), Err(e) => ui::toast_err(e) }
    });
    let cur_sign = if wh.is_object() { s(&wh, "sign") } else { "none".into() };
    let provoked: Vec<String> = wh["provoke"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let (s1, s2, s3, s4) = (set.clone(), set.clone(), set.clone(), set.clone());
    view! {
        <div class="caps" style="margin:28px 0 8px">"Webhooks this service sends to the app"</div>
        <div class="small t2">"How the send steps sign each webhook. The app checks it with the same secret."</div>
        <div class="row" style="gap:6px;margin-top:8px;flex-wrap:wrap">
            {[("none", "Not signed"), ("hex", "Hex HMAC"), ("hex_ts", "Hex + timestamp"), ("stripe", "Stripe t=,v1="), ("svix", "Svix v1,"), ("github", "GitHub sha256="), ("cloudevents", "CloudEvents ce-*")].into_iter().map(|(k, l)| { let s1 = s1.clone(); let on = cur_sign == k; view! {
                <button class="btn small" class:primary=on on:click=move |_| s1(json!({"webhook_signing": k}))>{l}</button> } }).collect_view()}
        </div>
        {wh.is_object().then(|| { let (secret, header) = (s(&wh, "secret"), s(&wh, "header")); view! {
            <div class="stack" style="margin-top:8px">
                <input class="input mono" placeholder="secret://payments-webhook" prop:value=secret on:change=move |e| s2(json!({"webhook_secret": event_target_value(&e)}))/>
                <input class="input mono" placeholder="header (each format has its own)" prop:value=header on:change=move |e| s3(json!({"webhook_header": event_target_value(&e)}))/></div>
            <div class="caps small" style="margin-top:10px">"Provoke on every send"</div>
            <div class="row" style="gap:6px;flex-wrap:wrap">{["wrong_signature", "old_timestamp", "no_signature", "twice", "out_of_order"].into_iter().map(|k| {
                let mut next = provoked.clone();
                let on = next.iter().any(|x| x == k);
                if on { next.retain(|x| x != k) } else { next.push(k.to_string()) }
                let s4 = s4.clone();
                view! { <button class="btn small" class:primary=on on:click=move |_| s4(json!({"webhook_provoke": next.clone()}))>{k.replace('_', " ")}</button> } }).collect_view()}</div>
        } })}
        <div class="caps" style="margin:22px 0 8px">"Idempotency"</div>
        <label class="check"><input type="checkbox" prop:checked=idem on:change=move |e| set(json!({"idempotency": event_target_checked(&e)}))/>"Repeat the first answer when a request comes again with the same Idempotency-Key (another body: 409)"</label>
        <div class="caps" style="margin:22px 0 8px">"The clock of this environment"</div>
        <div class="small t2">"Sends that wait (a tracking every 30 min, a renewal in 30 days) and now() in templates follow it."</div>
        <div class="mono" style="font-size:14px;margin:6px 0">{move || { let c = clock.get(); format!("{}  ·  {}", s(&c, "now"), if api::n(&c, "offsetMs") == 0 { "real time".to_string() } else { format!("real time + {}", api::span(api::n(&c, "offsetMs"))) }) }}</div>
        <div class="row" style="gap:6px;flex-wrap:wrap">{[("1h", "+ 1 h"), ("1d", "+ 1 day"), ("30d", "+ 30 days"), ("reset", "Back to real time")].into_iter().map(|(k, l)| view! { <button class="btn small" on:click=move |_| advance(k)>{l}</button> }).collect_view()}</div>
        {move || { let w = clock.get()["waiting"].as_array().cloned().unwrap_or_default(); (!w.is_empty()).then(|| view! {
            <table class="tbl" style="margin-top:8px"><tr><th>"When"</th><th>"What"</th><th>"To"</th></tr>
                {w.into_iter().map(|x| view! { <tr><td class="mono">{api::ago(api::n(&x, "due"))}</td><td class="mono">{s(&x, "event")}</td><td class="mono">{s(&x, "url")}</td></tr> }).collect_view()}</table> }) }}
    }
}
