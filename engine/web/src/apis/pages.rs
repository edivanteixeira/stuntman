//! C6 · Pages tab: the pages a service draws (declared in its plugin), edited as YAML with a live
//! preview, the page waiting for a person now, and the last answers (who authorized, denied, approved).

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui,
};

fn hhmmss(ms: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms as f64));
    format!("{:02}:{:02}:{:02}", d.get_hours(), d.get_minutes(), d.get_seconds())
}

#[component]
pub fn Pages() -> impl IntoView {
    let p = use_params_map();
    let qm = use_query_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let data = RwSignal::new(Value::Null);
    let sel = RwSignal::new(0usize);
    let yaml = RwSignal::new(String::new());
    let dirty = RwSignal::new(false);
    let waiting = RwSignal::new(Vec::<Value>::new());
    let last = RwSignal::new(Vec::<Value>::new());
    let stamp = RwSignal::new(0u32);
    Effect::new(move |_| {
        live.tick.track();
        let sv = service.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/plugins/{}/pages", enc(&sv))).await {
                if !dirty.get_untracked() {
                    yaml.set(s(&v["pages"][sel.get_untracked()], "yaml"));
                }
                data.set(v);
            }
            if let Ok(v) = api::get(&format!("/apis/steps?service={}", enc(&sv))).await { waiting.set(v["steps"].as_array().cloned().unwrap_or_default()); }
            if let Ok(v) = api::get(&format!("/apis/steps?service={}&all=true&limit=12", enc(&sv))).await {
                last.set(v["steps"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| s(x, "status") != "pending").collect());
            }
        });
    });
    let save = move || {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/pages/{}", enc(&sv), sel.get_untracked()), json!({"yaml": yaml.get_untracked()})).await {
                Ok(_) => { dirty.set(false); stamp.update(|x| *x += 1); ui::toast("Page saved") }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let save2 = save;
    let focus = move || qm.get().get("step");
    view! {
        <div class="page">
            <PluginHead service=service.get_untracked() active="pages" right=move || view! {
                <button class="btn primary" disabled=move || !dirty.get() on:click=move |_| save2()>"Save"</button> }/>
            <div class="body">
                <div class="subcol">
                    <div class="caps" style="padding:16px 20px 8px">"Pages of this service"</div>
                    {move || data.get()["pages"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, pg)| view! {
                        <div class="krow" class:on=move || sel.get() == i on:click=move |_| { sel.set(i); dirty.set(false); yaml.set(s(&data.get_untracked()["pages"][i], "yaml")) }>
                            <div style="font-weight:600">{s(&pg, "title")}</div>
                            <div class="small cap">{format!("{}{}", s(&pg, "route"), if pg["push"] == true { "  ·  or push" } else { "" })}</div>
                        </div>
                    }).collect_view()}
                    <div class="small cap" style="padding:16px 20px">"A page is what the provider shows in the browser: login, consent, approval. You declare fields and buttons; Stuntman draws it."</div>
                </div>
                <div class="pane pad" style="display:flex;gap:28px;align-items:flex-start">
                    <div style="width:420px;flex:none">
                        <div class="caps" style="margin-bottom:8px">"Page  ·  part of the plugin file"</div>
                        <textarea class="yaml-edit" spellcheck="false" prop:value=move || yaml.get() on:input=move |e| { yaml.set(event_target_value(&e)); dirty.set(true) }></textarea>
                        <div class="caps" style="margin:18px 0 6px">"Fields Stuntman draws"</div>
                        <div class="mono s12">"person  ·  otp  ·  text  ·  choice"</div>
                        <div class="small t2" style="margin-top:6px">"The first button authorizes (redirect with code and state), the second denies (error=access_denied). A push block answers at once and waits for Approve. No HTML, no scripts."</div>
                    </div>
                    <div style="width:340px;flex:none">
                        <div class="caps" style="margin-bottom:8px">"Preview  ·  live"</div>
                        {move || { let pv = s(&data.get()["pages"][sel.get()], "preview"); (!pv.is_empty()).then(|| view! {
                            <iframe class="preview" src=format!("{pv}?v={}", stamp.get())></iframe>
                            <a class="small" href=pv.clone() target="_blank">"Open it for real ›"</a> }) }}
                        <div class="caps" style="margin:22px 0 8px">"Waiting now"</div>
                        {move || { let w = waiting.get(); if w.is_empty() {
                            view! { <div class="small cap">"Nothing waits. The app opens the page when it starts a signature."</div> }.into_any()
                        } else { w.into_iter().map(|x| {
                            let ago = (js_sys::Date::now() as i64 - api::n(&x, "at")) / 1000;
                            let on = focus() == Some(s(&x, "id"));
                            view! { <div class="waitbox" class:on=on>
                                <div class="attn" style="font-weight:500">{if s(&x, "kind") == "push" { format!("Push approval for {} · {ago} s", s(&x, "person_name")) } else { format!("{} opened the page {ago} s ago", s(&x["params"], "client_id")) }}</div>
                                <div class="small"><a href=s(&x, "url") target="_blank">"Open page ›"</a><span class="t2">{if s(&x, "kind") == "push" { "   or approve_push via MCP" } else { "   or authorize_as via MCP" }}</span></div>
                            </div> }
                        }).collect_view().into_any() } }}
                        <div class="caps" style="margin:22px 0 8px">"Last answers"</div>
                        {move || last.get().into_iter().map(|x| view! {
                            <div class="row" style="align-items:flex-start;gap:12px;margin-bottom:8px">
                                <span class="mono small cap">{hhmmss(x["done_at"].as_i64().unwrap_or(api::n(&x, "at")))}</span>
                                <div><div class="s12" class:ok=["authorized", "approved"].contains(&s(&x, "status").as_str()) class:attn=!["authorized", "approved"].contains(&s(&x, "status").as_str())>
                                    {format!("{}{}", match s(&x, "status").as_str() { "authorized" => "Authorized", "approved" => "Approved", "denied" => "Denied", "rejected" => "Rejected", "locked" => "Locked after wrong codes", "expired" => "Expired", o => o }.to_string(),
                                        if s(&x, "person_name").is_empty() { String::new() } else { format!(" as {}", s(&x, "person_name")) })}</div>
                                <div class="small cap">{if s(&x, "by").is_empty() { "nobody".to_string() } else { s(&x, "by") }}</div></div>
                            </div>
                        }).collect_view()}
                    </div>
                </div>
            </div>
        </div>
    }
}
