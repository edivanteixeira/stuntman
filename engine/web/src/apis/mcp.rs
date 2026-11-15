//! A10b · a service as an MCP server: the tools it lists (from the routes with `tool:`), their
//! input schema and steps, how to point an agent at it, and the last calls.

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use serde_json::{json, Value};

use super::PluginHead;
use crate::{api::{self, enc, s}, ui};

#[component]
pub fn Tools() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let p = use_params_map();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let sv = RwSignal::new(Value::Null);
    let pick = RwSignal::new(String::new());
    let calls = RwSignal::new(Vec::<Value>::new());
    let (name, desc, schema) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new("{\"type\": \"object\", \"properties\": {}}".to_string()));
    Effect::new(move |_| {
        live.tick.track();
        let n = service.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/apis/services").await { sv.set(v["services"].as_array().and_then(|a| a.iter().find(|x| s(x, "name") == n).cloned()).unwrap_or(Value::Null)) }
            if let Ok(v) = api::get(&format!("/apis/calls?service={}", enc(&n))).await { calls.set(v["calls"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| s(c, "route").starts_with("MCP")).take(20).collect()) }
        });
    });
    let tools = Memo::new(move |_| sv.get()["mcp"]["tools"].as_array().cloned().unwrap_or_default());
    let cur = Memo::new(move |_| { let t = tools.get(); let k = pick.get(); t.iter().find(|x| s(x, "name") == k).cloned().or_else(|| t.first().cloned()).unwrap_or(Value::Null) });
    let add = move |_| {
        let n = service.get_untracked();
        let body = json!({"name": name.get_untracked(), "description": desc.get_untracked(), "input_schema": serde_json::from_str::<Value>(&schema.get_untracked()).unwrap_or(json!({"type": "object"}))});
        leptos::task::spawn_local(async move { match api::send("PUT", &format!("/plugins/{}/mcp-tools", enc(&n)), body).await { Ok(_) => ui::toast("Tool saved: give it steps in Routes"), Err(e) => ui::toast_err(e) } });
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="tools" right=|| ()/> }}
            <div class="body">
                <div class="list" style="width:240px">
                    <div class="caps" style="padding:16px 20px 6px">"Tools the server lists"</div>
                    {move || tools.get().into_iter().map(|t| { let nm = s(&t, "name"); let n2 = nm.clone(); view! {
                        <div class="mrow" class:on=move || s(&cur.get(), "name") == nm on:click=move |_| pick.set(n2.clone())><div class="mono">{s(&t, "name")}</div><div class="small cap">"tools/call"</div></div> } }).collect_view()}
                    <div class="small cap" style="padding:14px 20px">"The server also answers initialize, tools/list, ping and notifications. Sessions use Mcp-Session-Id."</div>
                </div>
                <div class="pane" style="padding:16px 24px;overflow:auto">
                    {move || { let t = cur.get(); (!t.is_null()).then(|| view! {
                        <div class="mono strong" style="font-size:16px">{s(&t, "name")}</div>
                        <div class="t2" style="margin:4px 0 10px">{s(&t, "description")}</div>
                        <div class="caps small">"Input schema"</div>
                        <pre class="yaml">{serde_json::to_string_pretty(&t["inputSchema"]).unwrap_or_default()}</pre>
                        <a class="link" href=format!("/apis/{}/routes", enc(&service.get()))>"Its steps are in Routes ›"</a> }) }}
                    <div class="caps" style="margin-top:18px">"Point the agent at"</div>
                    <div class="mono">{move || format!("{}{}", s(&sv.get()["urls"], "local"), s(&sv.get()["mcp"], "path"))}</div>
                    <div class="caps" style="margin-top:18px">"Last calls"</div>
                    <table class="tbl">{move || calls.get().into_iter().map(|c| view! { <tr><td class="mono">{api::hms(api::n(&c, "at"))}</td><td class="mono">{s(&c, "route")}</td><td>{s(&c, "sentence")}</td></tr> }).collect_view()}</table>
                    <div class="caps" style="margin-top:18px">"Add a tool"</div>
                    <div class="stack" style="max-width:620px"><div class="grid2"><input class="input mono" placeholder="search_docs" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
                        <input class="input" placeholder="what it does" prop:value=move || desc.get() on:input=move |e| desc.set(event_target_value(&e))/></div>
                        <textarea class="input mono" rows="3" prop:value=move || schema.get() on:input=move |e| schema.set(event_target_value(&e))></textarea>
                        <div><button class="btn primary small" on:click=add>"Save the tool"</button></div></div>
                    <div class="small cap" style="margin-top:12px">"Provoke: force “session_expired” (404), or an answer with rpc_error (-32602), from Answers."</div>
                </div>
            </div>
        </div>
    }
}
