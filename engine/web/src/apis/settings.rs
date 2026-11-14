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
                            <div class="caps" style="margin:28px 0 8px">"Remove"</div>
                            <div class="small t2">"Calls to this service become Unmatched. The plugin file is deleted from this environment."</div>
                            <button class="btn danger" style="margin-top:10px" on:click=move |_| remove.set(true)>"Remove this service"</button>
                        }
                    }}
                </div>
            </div>
            <Confirm open=remove question=Signal::derive(move || format!("Remove {}?", service.get())) detail=Signal::derive(|| "Calls to it become Unmatched; the plugin file is deleted from this environment.".to_string())
                action="Remove" on_yes=Callback::new(do_remove)/>
        </div>
    }
}
