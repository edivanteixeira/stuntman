//! A17 · Streams: Kafka topics with their messages (Avro decoded with the registry), consumer
//! groups and lag, schemas, producing as the partner would, provocations.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
pub fn Streams() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let msgs = RwSignal::new(Vec::<Value>::new());
    let groups = RwSignal::new(Vec::<Value>::new());
    let schemas = RwSignal::new(Value::Null);
    let (key, value, subject) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let new_topic = RwSignal::new(String::new());
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("messages".into()));
    let cur = Memo::new(move |_| qm.get().get("t").or_else(|| data.get()["topics"].as_array().and_then(|a| a.first().map(|x| s(x, "name")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let t = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/streams/topics").await { data.set(v) }
            if !t.is_empty() { if let Ok(v) = api::get(&format!("/streams/topics/{}/messages?limit=50", enc(&t))).await { msgs.set(v["messages"].as_array().cloned().unwrap_or_default()) } }
            if let Ok(v) = api::get("/streams/groups").await { groups.set(v["groups"].as_array().cloned().unwrap_or_default()) }
            if let Ok(v) = api::get("/streams/schemas").await { schemas.set(v) }
        });
    });
    let produce = move |_| {
        let t = cur.get_untracked();
        let v: Value = serde_json::from_str(&value.get_untracked()).unwrap_or(json!(value.get_untracked()));
        let mut body = json!({"key": key.get_untracked(), "value": v});
        if !subject.get_untracked().is_empty() { body["subject"] = json!(subject.get_untracked()); }
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/streams/topics/{}/produce", enc(&t)), body).await { Ok(r) => ui::toast(format!("Produced at partition {} offset {}", n(&r, "partition"), n(&r, "offset"))), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |k: &'static str| leptos::task::spawn_local(async move { match api::send("PUT", "/streams/config", json!({"provoke": k})).await { Ok(_) => ui::toast(if k == "none" { "Back to normal" } else { "Provoked" }), Err(e) => ui::toast_err(e) } });
    let create = move |_| { let t = new_topic.get_untracked(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/streams/topics/{}", enc(&t)), json!({"partitions": 3})).await { Ok(_) => ui::toast("Topic created"), Err(e) => ui::toast_err(e) } }) };
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Streams"</div><div class="small cap">"Kafka · Schema Registry"</div></div>
                {move || data.get()["topics"].as_array().cloned().unwrap_or_default().into_iter().map(|t| { let nm = s(&t, "name"); let n2 = nm.clone(); view! {
                    <a class="mrow" class:on=move || cur.get() == n2 href=format!("/streams?t={}", enc(&nm))><div class="row"><span class="mono">{s(&t, "name")}</span><span class="grow"></span><span class="mono small cap">{api::thousands(n(&t, "messages"))}</span></div>
                        <div class="small cap">{format!("{} partition{}", n(&t, "partitions"), if n(&t, "partitions") == 1 { "" } else { "s" })}</div></a> } }).collect_view()}
                <div class="stack" style="padding:12px 20px"><input class="input" placeholder="new topic" prop:value=move || new_topic.get() on:input=move |e| new_topic.set(event_target_value(&e))/><button class="btn small" on:click=create>"+ New topic"</button></div>
            </div>
            <div class="page">
                <AreaHead kind=Signal::derive(move || format!("Streams  ·  Kafka protocol  ·  {}  ·  Schema Registry {}", s(&data.get(), "bootstrap"), s(&data.get(), "schemaRegistry"))) title=Signal::derive(move || cur.get())
                    tabs=vec![("messages", "Messages", format!("/streams?t={}", enc(&cur.get_untracked()))), ("groups", "Consumer groups", format!("/streams?t={}&tab=groups", enc(&cur.get_untracked()))), ("schemas", "Schemas", format!("/streams?t={}&tab=schemas", enc(&cur.get_untracked())))]
                    counts=Signal::derive(move || vec![("groups", groups.get().len().to_string()), ("schemas", schemas.get()["subjects"].as_array().map(|a| a.len()).unwrap_or(0).to_string())])
                    active=match tab.get_untracked().as_str() { "groups" => "groups", "schemas" => "schemas", _ => "messages" }/>
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    {move || match tab.get().as_str() {
                        "groups" => view! { <table class="tbl"><tr><th>"Group"</th><th>"Members"</th><th>"Lag"</th><th>"State"</th><th></th></tr>
                            {groups.get().into_iter().map(|g| { let nm = s(&g, "group"); view! { <tr><td class="mono">{s(&g, "group")}</td>
                                <td>{g["members"].as_array().map(|a| a.iter().map(|m| format!("{}{}", s(m, "client"), if m["leader"].as_bool().unwrap_or(false) { " (leader)" } else { "" })).collect::<Vec<_>>().join(", ")).unwrap_or_default()}</td>
                                <td class="mono">{api::thousands(n(&g, "lag"))}</td><td class={if s(&g, "state") == "Stable" { "ok" } else { "att" }}>{format!("{} · gen {}", s(&g, "state"), n(&g, "generation"))}</td>
                                <td><button class="link" on:click=move |_| { let x = nm.clone(); leptos::task::spawn_local(async move { match api::send("POST", &format!("/streams/groups/{}/rebalance", enc(&x)), json!({})).await { Ok(_) => ui::toast("Rebalance started"), Err(e) => ui::toast_err(e) } }) }>"Rebalance"</button></td></tr> } }).collect_view()}</table> }.into_any(),
                        "schemas" => view! { <table class="tbl"><tr><th>"Subject"</th><th>"Versions"</th><th>"Latest id"</th></tr>
                            {schemas.get()["subjects"].as_array().cloned().unwrap_or_default().into_iter().map(|x| view! { <tr><td class="mono">{s(&x, "subject")}</td><td class="mono">{x["versions"].to_string()}</td><td class="mono">{n(&x, "latestId").to_string()}</td></tr> }).collect_view()}</table> }.into_any(),
                        _ => view! {
                            <table class="tbl"><tr><th>"Partition"</th><th>"Offset"</th><th>"Key"</th><th>"Value"</th><th>"When"</th></tr>
                                {msgs.get().into_iter().map(|m| { let v = &m["value"]; let shown = if v["value"].is_null() { s(v, "error") } else { v["value"].to_string() }; view! { <tr>
                                    <td class="mono">{n(&m, "partition").to_string()}</td><td class="mono">{n(&m, "offset").to_string()}</td><td class="mono">{s(&m, "key")}</td>
                                    <td class="mono small">{format!("{}{}", if v["schemaId"].is_null() { String::new() } else { format!("[schema {}] ", n(v, "schemaId")) }, shown.chars().take(160).collect::<String>())}</td><td class="mono">{api::hms(n(&m, "at"))}</td></tr> } }).collect_view()}</table>
                            <div class="caps" style="margin-top:12px">"Produce a message · as the partner would"</div>
                            <div class="stack" style="max-width:640px"><div class="grid2"><input class="input mono" placeholder="key" prop:value=move || key.get() on:input=move |e| key.set(event_target_value(&e))/>
                                <input class="input mono" placeholder="registry subject (Avro), optional" prop:value=move || subject.get() on:input=move |e| subject.set(event_target_value(&e))/></div>
                                <textarea class="input mono" rows="3" placeholder="{\"shipment\": \"SHP-1042\", \"status\": \"DELIVERED\"}" prop:value=move || value.get() on:input=move |e| value.set(event_target_value(&e))></textarea>
                                <div><button class="btn primary small" on:click=produce>"Produce"</button></div></div>
                        }.into_any(),
                    }}
                    <div class="caps" style="margin-top:14px">"Provoke"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">{[("leader_not_available", "Leader not available"), ("sasl_refused", "SASL refused"), ("slow_fetch", "Slow fetch (3 s)"), ("duplicate", "Duplicate delivery"), ("registry_down", "Registry down"), ("none", "Normal")].into_iter().map(|(k, l)| view! {
                        <button class="btn small" class:primary=move || s(&data.get(), "provoke") == k on:click=move |_| provoke(k)>{l}</button> }).collect_view()}</div>
                </div>
            </div>
        </div>
    }
}
