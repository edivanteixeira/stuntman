//! A22 · Queues › Event Hubs: an Event Hubs namespace on the AMQP port (and Kafka through
//! Streams' port). Its hubs, one cell per partition (begin – last sequence number), the events,
//! consumer groups with the receivers reading now (owner level, position) and BlobCheckpointStore
//! checkpoints, sending as another producer, provocations and the connection string.
//! A sibling of the namespace area, reached from the namespace dropdown like ActiveMQ.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
pub fn EventHubs() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let evs = RwSignal::new(Vec::<Value>::new());
    let (body, props, key, part) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (new_hub, new_parts) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("events".into()));
    let pick = Memo::new(move |_| qm.get().get("p"));
    let nsv = move || {
        let all = data.get()["namespaces"].as_array().cloned().unwrap_or_default();
        let want = qm.get().get("ns");
        all.iter().find(|x| Some(s(x, "namespace")) == want).or(all.first()).cloned().unwrap_or(Value::Null)
    };
    let ns = Memo::new(move |_| s(&nsv(), "namespace"));
    let hub = Memo::new(move |_| {
        let x = nsv();
        let hubs = x["hubs"].as_array().cloned().unwrap_or_default();
        let want = qm.get().get("h");
        hubs.iter().find(|h| Some(s(h, "name")) == want).or(hubs.first()).map(|h| s(h, "name")).unwrap_or_default()
    });
    let hv = move || nsv()["hubs"].as_array().and_then(|a| a.iter().find(|h| s(h, "name") == hub.get()).cloned()).unwrap_or(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let (nm, h, p) = (ns.get(), hub.get(), pick.get());
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/eventhubs").await { data.set(v) }
            if !nm.is_empty() && !h.is_empty() {
                let q = p.map(|p| format!("&partition={p}")).unwrap_or_default();
                if let Ok(v) = api::get(&format!("/eventhubs/{}/{}/events?last=50{q}", enc(&nm), enc(&h))).await { evs.set(v["events"].as_array().cloned().unwrap_or_default()) }
            }
        });
    });
    let link = move |h: &str, t: &str, p: Option<String>| format!("/queues/eventhubs?ns={}&h={}{}{}", enc(&ns.get_untracked()), enc(h), if t == "events" { String::new() } else { format!("&tab={t}") }, p.map(|p| format!("&p={p}")).unwrap_or_default());
    let send = move |_| {
        let mut b = json!({"body": body.get_untracked()});
        if !props.get_untracked().trim().is_empty() { b["properties"] = json!(props.get_untracked()); }
        if !key.get_untracked().trim().is_empty() { b["partition_key"] = json!(key.get_untracked()); }
        if !part.get_untracked().trim().is_empty() { b["partition"] = json!(part.get_untracked()); }
        let path = format!("/eventhubs/{}/{}/events", enc(&ns.get_untracked()), enc(&hub.get_untracked()));
        leptos::task::spawn_local(async move { match api::send("POST", &path, b).await { Ok(r) => ui::toast(format!("Sent to partition {} · #{}", s(&r, "partition"), r["sequenceNumbers"][0])), Err(e) => ui::toast_err(e) } });
    };
    let create = move |_| {
        let mut b = json!({"hub": new_hub.get_untracked()});
        if let Ok(p) = new_parts.get_untracked().trim().parse::<u32>() { b["partitions"] = json!(p); }
        let path = format!("/eventhubs/{}/hubs", enc(&ns.get_untracked()));
        leptos::task::spawn_local(async move { match api::send("POST", &path, b).await { Ok(_) => { ui::toast("Event hub created"); new_hub.set(String::new()) } Err(e) => ui::toast_err(e) } });
    };
    let set = move |b: Value, ok: &'static str| { let path = format!("/eventhubs/{}/settings", enc(&ns.get_untracked())); leptos::task::spawn_local(async move { match api::send("PUT", &path, b).await { Ok(_) => ui::toast(ok), Err(e) => ui::toast_err(e) } }) };
    let pv = move || nsv()["provoke"].clone();
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">{move || if ns.get().is_empty() { "Event Hubs".to_string() } else { ns.get() }}</div><div class="small cap">"Event Hubs · the namespace"</div></div>
                {move || nsv()["hubs"].as_array().cloned().unwrap_or_default().into_iter().map(|h| { let nm = s(&h, "name"); let n2 = nm.clone();
                    let groups = h["consumerGroups"].as_array().map(|a| a.len()).unwrap_or(0);
                    let sub = if n(&h, "partitionCount") == 1 { format!("1 partition · retention {}", s(&h, "retention")) } else { format!("{} partitions · {groups} group{}", n(&h, "partitionCount"), if groups == 1 { "" } else { "s" }) };
                    view! {
                    <a class="mrow" class:on=move || hub.get() == n2 href=link(&nm, "events", None)><div class="row"><span class="mono" style="overflow-wrap:anywhere;min-width:0">{nm.clone()}</span><span class="grow"></span>
                        <span class="mono small cap">{if n(&h, "events") == 0 { "—".to_string() } else { api::thousands(n(&h, "events")) }}</span></div>
                        <div class="small cap">{sub}</div></a> } }).collect_view()}
                <div class="stack" style="padding:12px 20px 4px;gap:8px">
                    <input class="input mono" placeholder="new event hub" prop:value=move || new_hub.get() on:input=move |e| new_hub.set(event_target_value(&e))/>
                    <input class="input mono" placeholder="partitions  4" prop:value=move || new_parts.get() on:input=move |e| new_parts.set(event_target_value(&e))/>
                    <button class="btn small" on:click=create disabled=move || ns.get().is_empty() || new_hub.get().trim().is_empty()>"+ New event hub"</button>
                </div>
                <div class="small cap" style="padding:8px 20px 4px">{move || if ns.get().is_empty() { "No Event Hubs namespace yet: create_eventhub makes one with its first hub.".to_string() } else { "Hubs are made with create_eventhub or here; retention runs on the environment clock.".to_string() }}</div>
                <a class="link" style="padding:4px 20px" href="/queues">"Service Bus and RabbitMQ ›"</a>
                {move || data.get()["bindError"].as_str().map(|e| view! { <div class="err" style="margin:6px 20px">{format!("Port {}: {e}", n(&data.get(), "port"))}</div> })}
            </div>
            <div class="page">
                {move || { let (h, t) = (hub.get(), tab.get()); view! {
                    <AreaHead kind=Signal::derive(move || format!("Queues  ·  Event Hubs  ·  {}  ·  AMQP :{}  ·  Kafka :{}", ns.get(), n(&data.get(), "port"), s(&nsv(), "kafka").split(':').nth(1).and_then(|x| x.split(' ').next()).unwrap_or("9092")))
                        title=Signal::derive(move || if hub.get().is_empty() { "Event Hubs".into() } else { hub.get() })
                        tabs=vec![("events", "Events", link(&h, "events", None)), ("groups", "Consumer groups", link(&h, "groups", None))]
                        counts=Signal::derive(move || { let x = hv(); vec![("events", api::thousands(n(&x, "events"))), ("groups", x["consumerGroups"].as_array().map(|a| a.len()).unwrap_or(0).to_string())] })
                        active=if t == "groups" { "groups" } else { "events" }/> } }}
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    {move || if tab.get() == "groups" {
                        let rows: Vec<(String, Value)> = hv()["consumerGroups"].as_array().cloned().unwrap_or_default().into_iter().flat_map(|g| {
                            let gn = s(&g, "name");
                            let mut parts: Vec<String> = g["readers"].as_array().into_iter().flatten().map(|r| s(r, "partition")).chain(g["checkpoints"].as_array().into_iter().flatten().map(|c| s(c, "partition"))).collect();
                            parts.sort(); parts.dedup();
                            if parts.is_empty() { parts.push(String::new()) }
                            parts.into_iter().map(move |p| (gn.clone(), json!({"p": p, "r": g["readers"].as_array().and_then(|a| a.iter().find(|r| s(r, "partition") == p).cloned()), "c": g["checkpoints"].as_array().and_then(|a| a.iter().find(|c| s(c, "partition") == p).cloned())}))).collect::<Vec<_>>()
                        }).collect();
                        view! {
                            <div class="caps">"Receivers now and checkpoints in Blob Storage"</div>
                            <table class="tbl"><tr><th>"Group"</th><th>"Part."</th><th>"Receiver"</th><th>"Owner level"</th><th>"Position"</th><th>"Checkpoint"</th><th>"Since"</th></tr>
                                {rows.into_iter().map(|(g, x)| { let (r, c) = (&x["r"], &x["c"]); let dash = || "—".to_string();
                                    let behind = !r.is_null() && !c.is_null() && c["sequenceNumber"].as_str().and_then(|v| v.parse::<i64>().ok()).or(c["sequenceNumber"].as_i64()).unwrap_or(-1) < n(r, "position");
                                    view! { <tr><td>{g}</td><td class="mono">{if s(&x, "p").is_empty() { dash() } else { s(&x, "p") }}</td>
                                    <td class="mono">{if r.is_null() { dash() } else { s(r, "client") }}</td>
                                    <td>{if r.is_null() || r["epoch"].is_null() { dash() } else { format!("epoch {}", n(r, "epoch")) }}</td>
                                    <td class="mono">{if r.is_null() || n(r, "position") < 0 { dash() } else { n(r, "position").to_string() }}</td>
                                    <td class="mono" class:ok={!c.is_null() && !behind} class:attn=behind class:cap={c.is_null()}>{if c.is_null() { "no checkpoint".to_string() } else { format!("{} · offset {}", s(c, "sequenceNumber"), s(c, "offset")) }}</td>
                                    <td class="mono">{if r.is_null() { dash() } else { api::hms(n(r, "since")) }}</td></tr> } }).collect_view()}</table>
                            <div class="small cap">"Checkpoints are the blobs BlobCheckpointStore writes (<namespace>/<hub>/<group>/checkpoint/<partition>) to a Storage account."</div>
                        }.into_any()
                    } else {
                        view! {
                            <div class="caps">"Partitions · begin – last sequence number · lookup3 of the partition key picks one"</div>
                            <div class="row" style="gap:8px;flex-wrap:wrap">{move || hv()["partitions"].as_array().cloned().unwrap_or_default().into_iter().map(|p| { let id = s(&p, "id"); let on = pick.get().as_deref() == Some(id.as_str());
                                let to = if on { link(&hub.get_untracked(), "events", None) } else { link(&hub.get_untracked(), "events", Some(id.clone())) };
                                view! { <a href=to style=format!("width:222px;padding:8px 12px;border-radius:6px;text-decoration:none;color:inherit;border:1px solid {}", if on { "var(--ink)" } else { "var(--border)" })>
                                    <div class="row"><span class="strong small">{format!("Partition {id}")}</span><span class="grow"></span><span class="mono small cap">{n(&p, "count").to_string()}</span></div>
                                    <div class="mono small cap">{if p["isEmpty"].as_bool().unwrap_or(true) && n(&p, "lastEnqueuedSequenceNumber") < 0 { "empty".to_string() } else { format!("{} – {}", n(&p, "beginSequenceNumber"), n(&p, "lastEnqueuedSequenceNumber")) }}</div></a> } }).collect_view()}</div>
                            <table class="tbl"><tr><th>"Part."</th><th>"Seq"</th><th>"Offset"</th><th>"Partition key"</th><th>"Body"</th><th>"Properties"</th><th>"By"</th><th>"When"</th></tr>
                                {move || evs.get().into_iter().map(|e| {
                                    let b = if e["body"].is_null() { format!("{} bytes (base64)", s(&e, "bodyBase64").len() * 3 / 4) } else { s(&e, "body") };
                                    let pr = e["properties"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().map(String::from).unwrap_or(v.to_string()))).collect::<Vec<_>>().join(", ")).filter(|x| !x.is_empty()).unwrap_or("—".into());
                                    let (who, full) = if s(&e, "via") == "amqp" || s(&e, "via") == "kafka" { (if s(&e, "via") == "kafka" { "kafka".to_string() } else { "app".to_string() }, s(&e, "sender")) } else { (s(&e, "sender"), String::new()) };
                                    view! { <tr><td class="mono">{s(&e, "partition")}</td><td class="mono">{n(&e, "sequenceNumber").to_string()}</td><td class="mono">{s(&e, "offset")}</td>
                                    <td class="mono">{if e["partitionKey"].is_null() { "—".to_string() } else { s(&e, "partitionKey") }}</td>
                                    <td class="mono small" title=b.clone()>{b.chars().take(90).collect::<String>()}</td><td class="mono small">{pr}</td><td title=full>{who}</td><td class="mono">{api::hms(n(&e, "enqueuedAt"))}</td></tr> } }).collect_view()}</table>
                            {move || evs.get().is_empty().then(|| view! { <div class="small cap">"No event yet."</div> })}
                            <div class="caps" style="margin-top:12px">"Send events · as another producer would"</div>
                            <div class="stack" style="max-width:640px">
                                <textarea class="input mono" rows="2" placeholder="{\"orderId\": \"ORD-1043\", \"status\": \"PAID\"}" prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea>
                                <div class="row" style="gap:8px"><input class="input mono" style="width:316px" placeholder="properties {\"kind\": \"OrderPaid\"}" prop:value=move || props.get() on:input=move |e| props.set(event_target_value(&e))/>
                                    <input class="input mono" style="width:200px" placeholder="partition key  ORD-1043" prop:value=move || key.get() on:input=move |e| key.set(event_target_value(&e))/>
                                    <input class="input mono" style="width:108px" placeholder="partition" prop:value=move || part.get() on:input=move |e| part.set(event_target_value(&e))/></div>
                                <div><button class="btn primary small" on:click=send disabled=move || hub.get().is_empty()>"Send"</button></div></div>
                        }.into_any()
                    }}
                    <div class="caps" style="margin-top:14px">"Provoke"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">
                        <button class="btn small" class:primary=move || { n(&pv(), "server_busy") > 0 } on:click=move |_| set(json!({"server_busy": 2}), "The next 2 sends get ServerBusy")>"Server busy (next 2)"</button>
                        <button class="btn small" class:primary=move || { n(&pv(), "quota_exceeded") > 0 } on:click=move |_| set(json!({"quota_exceeded": 10}), "The next 10 sends get QuotaExceeded")>"Quota exceeded"</button>
                        <button class="btn small" class:primary=move || { n(&pv(), "slow_sends_ms") > 0 } on:click=move |_| set(json!({"slow_sends": "2s"}), "Sends wait 2 s")>"Slow sends (2 s)"</button>
                        <button class="btn small" class:primary=move || { pv()["not_found"].as_array().is_some_and(|a| !a.is_empty()) } on:click=move |_| set(json!({"not_found": [hub.get_untracked()]}), "This hub answers not found")>"Not found"</button>
                        <button class="btn small" on:click=move |_| set(json!({"steal_owner": hub.get_untracked()}), "Receivers disconnected (link stolen)")>"Steal owner"</button>
                        <button class="btn small" on:click=move |_| set(json!({"drop_connections": true}), "Connections dropped")>"Drop connections"</button>
                        <button class="btn small" class:primary=move || { let x = pv(); !x.is_null() && n(&x, "server_busy") + n(&x, "quota_exceeded") + n(&x, "slow_sends_ms") == 0 && x["not_found"].as_array().is_none_or(|a| a.is_empty()) }
                            on:click=move |_| set(json!({"reset": true}), "Back to normal")>"Normal"</button>
                    </div>
                    {move || { let x = pv(); let mut what = vec![];
                        if n(&x, "server_busy") > 0 { what.push(format!("the next {} sends answer com.microsoft:server-busy; the SDKs retry", n(&x, "server_busy"))) }
                        if n(&x, "quota_exceeded") > 0 { what.push(format!("the next {} sends answer QuotaExceeded", n(&x, "quota_exceeded"))) }
                        if n(&x, "slow_sends_ms") > 0 { what.push(format!("each send waits {} ms", n(&x, "slow_sends_ms"))) }
                        if x["not_found"].as_array().is_some_and(|a| !a.is_empty()) { what.push(format!("not found: {}", x["not_found"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))) }
                        (!what.is_empty()).then(|| view! { <div class="small attn">{what.join("  ·  ")}</div> }) }}
                    <div class="mono small cap" style="overflow-wrap:anywhere;cursor:pointer" title="Copy" on:click=move |_| ui::copy(&s(&hv(), "connection"), "Connection string")>{move || s(&hv(), "connection")}</div>
                    <div class="mono small cap">{move || format!("Kafka: {} · topic {}", s(&nsv(), "kafka"), hub.get())}</div>
                </div>
            </div>
        </div>
    }
}
