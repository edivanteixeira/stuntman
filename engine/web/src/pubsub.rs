//! A20 · Pub/Sub: the emulator (gRPC, PUBSUB_EMULATOR_HOST=localhost:8085). Topics with their
//! schema, messages and the state of each in every subscription, subscriptions with their type
//! (pull, push, BigQuery, Cloud Storage), filter and ack counts, snapshots, publishing as another
//! service would, creating a subscription, provocations on Publish.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Map, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

fn short(full: &str) -> String {
    full.rsplit('/').next().unwrap_or(full).to_string()
}

fn project(full: &str) -> String {
    full.split('/').nth(1).unwrap_or_default().to_string()
}

/// pull | push | BigQuery | Cloud Storage, as the subscription delivers.
fn kind_label(x: &Value) -> String {
    match s(x, "type").as_str() { "push" => "push".into(), "bigquery" => "BigQuery".into(), "cloud_storage" => "Cloud Storage".into(), "bigtable" => "Bigtable".into(), _ => "pull".into() }
}

fn ingestion_label(k: &str) -> &'static str {
    match k { "aws_kinesis" => "Amazon Kinesis", "cloud_storage" => "Cloud Storage", "azure_event_hubs" => "Azure Event Hubs", "aws_msk" => "Amazon MSK", "confluent_cloud" => "Confluent Cloud", _ => "source" }
}

/// Where a push, BigQuery, Cloud Storage or Bigtable subscription writes.
fn kind_detail(x: &Value) -> String {
    let d = &x["delivery"];
    match s(x, "type").as_str() { "push" => s(d, "endpoint"), "bigquery" => s(d, "table"), "cloud_storage" => format!("gs://{} · {}", s(d, "bucket"), s(d, "format")), "bigtable" => s(d, "table"), _ => String::new() }
}

#[component]
pub fn PubSub() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let msgs = RwSignal::new(Vec::<Value>::new());
    let (body, attrs, key) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (sub_name, sub_deadline, sub_dl, sub_order) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(false));
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("messages".into()));
    let cur = Memo::new(move |_| qm.get().get("t").or_else(|| data.get()["topics"].as_array().and_then(|a| a.first().map(|x| s(x, "name")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let t = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/pubsub/topics").await { data.set(v) }
            if !t.is_empty() { if let Ok(v) = api::get(&format!("/pubsub/messages?topic={}&limit=50", enc(&t))).await { msgs.set(v["messages"].as_array().cloned().unwrap_or_default()) } }
        });
    });
    let topic = move || data.get()["topics"].as_array().and_then(|a| a.iter().find(|x| s(x, "name") == cur.get()).cloned()).unwrap_or(Value::Null);
    let publish = move |_| {
        let t = cur.get_untracked();
        let a: Map<String, Value> = attrs.get_untracked().split(',').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), json!(v.trim()))).collect();
        let b = json!({"data": body.get_untracked(), "attributes": a, "ordering_key": key.get_untracked(), "project": project(&t)});
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/pubsub/topics/{}/publish", enc(&short(&t))), b).await { Ok(r) => ui::toast(format!("Published message {}", s(&r, "messageId"))), Err(e) => ui::toast_err(e) } });
    };
    let create = move |_| {
        let t = cur.get_untracked();
        let mut b = json!({"topic": t, "project": project(&t), "ordering": sub_order.get_untracked()});
        if let Ok(d) = sub_deadline.get_untracked().trim().trim_end_matches('s').trim().parse::<u64>() { b["ack_deadline_seconds"] = json!(d); }
        if !sub_dl.get_untracked().trim().is_empty() { b["dead_letter_topic"] = json!(sub_dl.get_untracked().trim()); }
        let nm = sub_name.get_untracked();
        leptos::task::spawn_local(async move { match api::send("PUT", &format!("/pubsub/subscriptions/{}", enc(nm.trim())), b).await { Ok(_) => ui::toast("Subscription created"), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |k: &'static str| leptos::task::spawn_local(async move { match api::send("PUT", "/pubsub/config", json!({"provoke": k})).await { Ok(_) => ui::toast(if k == "none" { "Back to normal" } else { "Provoked" }), Err(e) => ui::toast_err(e) } });
    let provoked = move || s(&data.get()["provoke"], "kind");
    let link = move |t: &str, tab: &str| format!("/pubsub?t={}{}", enc(t), if tab == "messages" { String::new() } else { format!("&tab={tab}") });
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Pub/Sub"</div><div class="small cap">"Emulator · gRPC"</div></div>
                {move || data.get()["topics"].as_array().cloned().unwrap_or_default().into_iter().map(|t| { let nm = s(&t, "name"); let n2 = nm.clone(); let subs = t["subscriptions"].as_array().map(|a| a.len()).unwrap_or(0); view! {
                    <a class="mrow" class:on=move || cur.get() == n2 href=link(&nm, "messages")><div class="row"><span class="mono">{short(&nm)}</span><span class="grow"></span><span class="mono small cap">{api::thousands(n(&t, "messages"))}</span></div>
                        <div class="small cap">{format!("{} · {subs} subscription{}", project(&nm), if subs == 1 { "" } else { "s" })}</div></a> } }).collect_view()}
                <div class="small cap" style="padding:12px 20px">"Topics appear when the app makes them, or when you publish."</div>
            </div>
            <div class="page">
                {move || { let (t, tb) = (cur.get(), tab.get()); view! {
                    <AreaHead kind=Signal::derive(move || format!("Pub/Sub  ·  emulator  ·  PUBSUB_EMULATOR_HOST={}  ·  project {}", s(&data.get()["connect"], "PUBSUB_EMULATOR_HOST"), s(&data.get(), "project")))
                        title=Signal::derive(move || if cur.get().is_empty() { "Pub/Sub".into() } else { short(&cur.get()) })
                        tabs=vec![("messages", "Messages", link(&t, "messages")), ("subscriptions", "Subscriptions", link(&t, "subscriptions"))]
                        counts=Signal::derive(move || { let x = topic(); vec![("messages", api::thousands(n(&x, "messages"))), ("subscriptions", x["subscriptions"].as_array().map(|a| a.len()).unwrap_or(0).to_string())] })
                        active=if tb == "subscriptions" { "subscriptions" } else { "messages" }/> } }}
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    {move || { let x = topic(); (!x["schema"].is_null()).then(|| view! {
                        <div class="small"><span class="caps">"Schema  "</span><span class="mono">{format!("{} · {}", short(&s(&x["schema"], "name")), s(&x["schema"], "encoding"))}</span>
                            <span class="cap">"  ·  every publish is validated; subscribers get googclient_schemaname, googclient_schemarevisionid"</span></div> }) }}
                    {move || { let x = topic(); let ing = x["ingestion"].clone(); let tr: Vec<String> = x["transforms"].as_array().cloned().unwrap_or_default().iter().map(|v| v.as_str().unwrap_or_default().to_string()).collect();
                        (!ing.is_null() || !tr.is_empty()).then(|| view! { <div class="small">
                            {(!ing.is_null()).then(|| view! { <span class="caps">"Ingestion  "</span><span class="mono">{format!("{} · {}", ingestion_label(&s(&ing, "kind")), s(&ing, "from"))}</span>
                                <span class=if s(&ing, "state") == "ACTIVE" { "ok" } else { "attn" }>{format!("  {}", s(&ing, "state"))}</span><span class="cap">"    "</span> })}
                            {(!tr.is_empty()).then(|| view! { <span class="caps">"Transforms  "</span><span class="mono">{tr.join(", ")}</span> })}</div> }) }}
                    {move || if tab.get() == "subscriptions" {
                        view! {
                            <table class="tbl"><tr><th>"Subscription"</th><th>"Type"</th><th>"Filter"</th><th>"Ack deadline"</th><th>"Ordering"</th><th>"Dead-letter"</th><th>"Pending"</th><th>"Outstanding"</th><th>"Acked"</th><th>"Dead"</th></tr>
                                {topic()["subscriptions"].as_array().cloned().unwrap_or_default().into_iter().map(|x| view! { <tr><td class="mono" title=s(&x, "name")>{short(&s(&x, "name"))}{x["detached"].as_bool().unwrap_or(false).then(|| view! { <span class="small cap">" · detached"</span> })}</td>
                                    <td>{kind_label(&x)}{(!kind_detail(&x).is_empty()).then(|| view! { <div class="mono small cap">{kind_detail(&x)}</div> })}
                                        {(!x["lastError"].is_null()).then(|| view! { <div class="small danger" title=s(&x["lastError"], "error")>{s(&x["lastError"], "error").chars().take(60).collect::<String>()}</div> })}</td><td class="mono small">{if x["filter"].is_null() { "—".to_string() } else { s(&x, "filter") }}</td>
                                    <td class="mono">{format!("{} s", n(&x, "ackDeadlineSeconds"))}</td><td>{if x["messageOrdering"].as_bool().unwrap_or(false) { "ordered" } else { "—" }}</td>
                                    <td class="mono">{if x["deadLetterTopic"].is_null() { "—".to_string() } else { format!("{} · {}", short(&s(&x, "deadLetterTopic")), n(&x, "maxDeliveryAttempts")) }}</td>
                                    <td class="mono">{n(&x, "pending").to_string()}</td><td class="mono">{n(&x, "outstanding").to_string()}</td><td class="mono">{n(&x, "acked").to_string()}</td>
                                    <td class="mono" class:danger={n(&x, "deadLettered") > 0}>{n(&x, "deadLettered").to_string()}</td></tr> }).collect_view()}</table>
                            {move || { let t = cur.get(); let snaps: Vec<Value> = data.get()["snapshots"].as_array().cloned().unwrap_or_default().into_iter().filter(|x| s(x, "topic") == t).collect();
                                (!snaps.is_empty()).then(|| view! {
                                    <div class="caps" style="margin-top:12px">"Snapshots"</div>
                                    <table class="tbl"><tr><th>"Snapshot"</th><th>"Taken from"</th><th>"Unacked then"</th><th>"Taken"</th><th>"Expires"</th></tr>
                                        {snaps.into_iter().map(|x| view! { <tr><td class="mono" title=s(&x, "name")>{short(&s(&x, "name"))}</td><td class="mono">{short(&s(&x, "subscription"))}</td>
                                            <td class="mono">{n(&x, "unackedAtSnapshot").to_string()}</td><td class="mono">{api::hms(n(&x, "createdAt"))}</td><td class="mono">{api::hms(n(&x, "expiresAt"))}</td></tr> }).collect_view()}</table> }) }}
                            <div class="caps" style="margin-top:12px">"Create a subscription"</div>
                            <div class="row" style="gap:8px;flex-wrap:wrap;align-items:center">
                                <input class="input mono" style="width:200px" placeholder="new subscription" prop:value=move || sub_name.get() on:input=move |e| sub_name.set(event_target_value(&e))/>
                                <input class="input mono" style="width:90px" placeholder="10 s" prop:value=move || sub_deadline.get() on:input=move |e| sub_deadline.set(event_target_value(&e))/>
                                <input class="input mono" style="width:220px" placeholder="dead-letter topic, optional" prop:value=move || sub_dl.get() on:input=move |e| sub_dl.set(event_target_value(&e))/>
                                <label class="row" style="gap:6px"><input type="checkbox" prop:checked=move || sub_order.get() on:change=move |e| sub_order.set(event_target_checked(&e))/>"ordering"</label>
                                <button class="btn small" on:click=create disabled=move || cur.get().is_empty()>"Create subscription"</button></div>
                        }.into_any()
                    } else {
                        view! {
                            <table class="tbl"><tr><th>"Id"</th><th>"Data"</th><th>"Attributes"</th><th>"Ordering key"</th><th>"By"</th><th>"In each subscription"</th><th>"When"</th></tr>
                                {msgs.get().into_iter().map(|m| {
                                    let d = &m["data"]["value"]; let data = d.as_str().map(String::from).unwrap_or(d.to_string());
                                    let at = m["attributes"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default())).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                                    let subs = m["subscriptions"].as_array().cloned().unwrap_or_default();
                                    let states: Vec<String> = subs.iter().map(|x| s(x, "state")).collect();
                                    let cl = if states.iter().any(|x| x == "dead") { "danger" } else if states.iter().any(|x| x == "pending" || x == "expired") { "attn" } else if !states.is_empty() && states.iter().all(|x| x == "acked") { "ok" } else { "" };
                                    let line = subs.iter().map(|x| format!("{} {}{}", short(&s(x, "subscription")), s(x, "state"), if n(x, "deliveryAttempts") > 1 { format!(" ({})", n(x, "deliveryAttempts")) } else { String::new() })).collect::<Vec<_>>().join(" · ");
                                    let by = s(&m, "publisher"); let by = by.strip_prefix("app:").map(|_| "app".to_string()).unwrap_or(by);
                                    view! { <tr><td class="mono">{s(&m, "messageId")}</td><td class="mono small" title=data.clone()>{data.chars().take(90).collect::<String>()}</td>
                                        <td class="mono small">{at}</td><td class="mono">{s(&m, "orderingKey")}</td><td>{by}</td><td class=cl>{if line.is_empty() { "no subscription".to_string() } else { line }}</td>
                                        <td class="mono">{api::hms(n(&m, "publishedAt"))}</td></tr> } }).collect_view()}</table>
                            {msgs.get().is_empty().then(|| view! { <div class="small cap">"No message yet."</div> })}
                            <div class="caps" style="margin-top:12px">"Publish a message · as another service would"</div>
                            <div class="stack" style="max-width:640px">
                                <textarea class="input mono" rows="2" placeholder="{\"shipment\": \"SHP-1043\", \"status\": \"DELIVERED\"}" prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea>
                                <div class="grid2"><input class="input mono" placeholder="attributes  kind=status, carrier=globex" prop:value=move || attrs.get() on:input=move |e| attrs.set(event_target_value(&e))/>
                                    <input class="input mono" placeholder="ordering key" prop:value=move || key.get() on:input=move |e| key.set(event_target_value(&e))/></div>
                                <div><button class="btn primary small" on:click=publish disabled=move || cur.get().is_empty()>"Publish"</button></div></div>
                        }.into_any()
                    }}
                    <div class="caps" style="margin-top:14px">"Provoke on publish"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">{[("unavailable", "Unavailable"), ("deadline_exceeded", "Deadline exceeded"), ("resource_exhausted", "Resource exhausted"), ("not_found", "Not found"), ("slow_publish", "Slow publish (3 s)"), ("duplicate", "Duplicate"), ("ack_transient", "Ack transient"), ("none", "Normal")].into_iter().map(|(k, l)| view! {
                        <button class="btn small" class:primary=move || if k == "none" { provoked().is_empty() && !data.get().is_null() } else { provoked() == k } on:click=move |_| provoke(k)>{l}</button> }).collect_view()}</div>
                    <div class="mono small cap">{move || format!("export PUBSUB_EMULATOR_HOST={}   ·   any project id works; short names use {}", s(&data.get()["connect"], "PUBSUB_EMULATOR_HOST"), s(&data.get(), "project"))}</div>
                </div>
            </div>
        </div>
    }
}
