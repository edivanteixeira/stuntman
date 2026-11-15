//! A19 · Queues › ActiveMQ: the OpenWire broker (port 61616). Its destinations with messages per
//! state, the consumers connected now (selector, durable subscription), a topic's durable
//! subscriptions, the transactions, sending as the partner would, provocations and users.
//! A sibling of the namespace area: ActiveMQ has no namespaces, one broker per environment.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
pub fn ActiveMq() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let msgs = RwSignal::new(Vec::<Value>::new());
    let (text, props, corr, users) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("messages".into()));
    let cur = Memo::new(move |_| qm.get().get("d").or_else(|| data.get()["destinations"].as_array().and_then(|a| a.first().map(|x| s(x, "name")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let d = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/activemq/destinations").await { data.set(v) }
            if !d.is_empty() { if let Ok(v) = api::get(&format!("/activemq/messages?destination={}&last=50", enc(&d))).await { msgs.set(v["messages"].as_array().cloned().unwrap_or_default()) } }
        });
    });
    let dest = move || data.get()["destinations"].as_array().and_then(|a| a.iter().find(|x| s(x, "name") == cur.get()).cloned()).unwrap_or(Value::Null);
    let send = move |_| {
        let mut body = json!({"destination": cur.get_untracked(), "text": text.get_untracked()});
        if !props.get_untracked().trim().is_empty() { body["properties"] = json!(props.get_untracked()); }
        if !corr.get_untracked().trim().is_empty() { body["correlation_id"] = json!(corr.get_untracked()); }
        leptos::task::spawn_local(async move { match api::send("POST", "/activemq/messages", body).await { Ok(r) => ui::toast(format!("Sent #{}", r["seqs"][0])), Err(e) => ui::toast_err(e) } });
    };
    let set = move |body: Value, ok: &'static str| leptos::task::spawn_local(async move { match api::send("PUT", "/activemq/settings", body).await { Ok(_) => ui::toast(ok), Err(e) => ui::toast_err(e) } });
    let st = move || data.get()["settings"].clone();
    let link = move |d: &str, t: &str| format!("/queues/activemq?d={}{}", enc(d), if t == "messages" { String::new() } else { format!("&tab={t}") });
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"ActiveMQ"</div><div class="small cap">"OpenWire · the broker"</div></div>
                {move || data.get()["destinations"].as_array().cloned().unwrap_or_default().into_iter().map(|d| { let nm = s(&d, "name"); let n2 = nm.clone(); let c = &d["counts"];
                    let consumers = d["consumers"].as_array().map(|a| a.len()).unwrap_or(0);
                    let who = if s(&d, "kind") == "topic" { "subscriber" } else { "consumer" };
                    view! {
                    <a class="mrow" class:on=move || cur.get() == n2 href=link(&nm, "messages")><div class="row"><span class="mono" style="overflow-wrap:anywhere;min-width:0">{nm.clone()}</span><span class="grow"></span>
                        <span class="mono small" class:attn={n(c, "pending") > 0} class:cap={n(c, "pending") == 0}>{if n(c, "pending") == 0 { "—".to_string() } else { n(c, "pending").to_string() }}</span></div>
                        <div class="small cap">{format!("{} · {}", s(&d, "kind"), if consumers == 0 { format!("no {who}") } else { format!("{consumers} {who}{}", if consumers == 1 { "" } else { "s" }) })}</div></a> } }).collect_view()}
                <div class="small cap" style="padding:12px 20px 4px">"Destinations appear when the app connects, or when you send."</div>
                <a class="link" style="padding:4px 20px" href="/queues">"Service Bus and RabbitMQ ›"</a>
                {move || data.get()["bindError"].as_str().map(|e| view! { <div class="err" style="margin:6px 20px">{format!("Port {}: {e}", n(&data.get(), "port"))}</div> })}
            </div>
            <div class="page">
                {move || { let (d, t) = (cur.get(), tab.get()); view! {
                    <AreaHead kind=Signal::derive(move || format!("Queues  ·  ActiveMQ  ·  OpenWire  ·  tcp://localhost:{}", n(&data.get(), "port"))) title=Signal::derive(move || if cur.get().is_empty() { "ActiveMQ".into() } else { cur.get() })
                        tabs=vec![("messages", "Messages", link(&d, "messages")), ("consumers", "Consumers", link(&d, "consumers"))]
                        counts=Signal::derive(move || { let x = dest(); let c = &x["counts"]; vec![("messages", c.as_object().map(|o| o.values().filter_map(|v| v.as_i64()).sum::<i64>()).unwrap_or(0).to_string()), ("consumers", x["consumers"].as_array().map(|a| a.len()).unwrap_or(0).to_string())] })
                        active=if t == "consumers" { "consumers" } else { "messages" }/> } }}
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    {move || if tab.get() == "consumers" {
                        let subs = dest()["subscriptions"].as_array().cloned();
                        let txs = data.get()["transactions"].as_array().cloned().unwrap_or_default();
                        view! { <table class="tbl"><tr><th>"Consumer"</th><th>"User"</th><th>"Selector"</th><th>"Prefetch"</th><th>"From"</th><th>"Since"</th></tr>
                            {dest()["consumers"].as_array().cloned().unwrap_or_default().into_iter().map(|c| view! { <tr><td class="mono" title=s(&c, "id")>{format!("…{}", s(&c, "id").rsplit('-').next().unwrap_or_default())}
                                {(!c["durable"].is_null()).then(|| view! { <span class="small cap">{format!("  durable {}", s(&c, "durable"))}</span> })}
                                {c["exclusive"].as_bool().unwrap_or(false).then(|| view! { <span class="small cap">"  exclusive"</span> })}
                                {c["noLocal"].as_bool().unwrap_or(false).then(|| view! { <span class="small cap">"  noLocal"</span> })}</td><td>{s(&c, "user")}</td>
                                <td class="mono small">{if s(&c, "selector").is_empty() { "—".to_string() } else { s(&c, "selector") }}</td>
                                <td class="mono">{n(&c, "prefetch").to_string()}</td><td class="mono">{s(&c, "remote")}<span class="small cap">{format!("  {}", s(&c, "wire"))}</span></td><td class="mono">{api::hms(n(&c, "since"))}</td></tr> }).collect_view()}</table>
                            {dest()["consumers"].as_array().is_none_or(|a| a.is_empty()).then(|| view! { <div class="small cap">"No consumer on this destination now."</div> })}
                            {subs.map(|subs| view! {
                                <div class="caps" style="margin-top:12px">"Durable subscriptions · kept while offline"</div>
                                <table class="tbl"><tr><th>"Subscription"</th><th>"Client ID"</th><th>"Selector"</th><th>"Kept"</th><th>"State"</th><th>"Last seen"</th></tr>
                                    {subs.iter().map(|x| { let on = x["active"].as_bool().unwrap_or(false); view! { <tr><td class="mono">{s(x, "name")}</td><td class="mono">{s(x, "clientId")}</td>
                                        <td class="mono small">{if s(x, "selector").is_empty() { "—".to_string() } else { s(x, "selector") }}</td>
                                        <td class="mono" class:attn={n(x, "pending") > 0}>{n(x, "pending").to_string()}</td><td class:ok=on>{if on { "online" } else { "offline" }}</td><td class="mono">{api::hms(n(x, "lastSeen"))}</td></tr> } }).collect_view()}</table>
                                {subs.is_empty().then(|| view! { <div class="small cap">"No durable subscription: a consumer with a subscription name and a client ID makes one."</div> })} })}
                            <div class="caps" style="margin-top:12px">"Transactions · local and XA"</div>
                            <table class="tbl"><tr><th>"Transaction"</th><th>"Kind"</th><th>"State"</th><th>"Sends"</th><th>"Acks"</th><th>"User"</th><th>"Started"</th></tr>
                                {txs.iter().take(10).map(|t| { let st = s(t, "state"); let cl = match st.as_str() { "prepared" | "active" | "ended" => "attn", "committed" => "ok", _ => "" };
                                    view! { <tr><td class="mono small" title=s(t, "id")>{s(t, "id").chars().take(36).collect::<String>()}</td><td>{s(t, "kind")}</td><td class=cl>{st}</td>
                                    <td class="mono">{n(t, "sends").to_string()}</td><td class="mono">{n(t, "acks").to_string()}</td><td>{s(t, "user")}</td><td class="mono">{api::hms(n(t, "startedAt"))}</td></tr> } }).collect_view()}</table>
                            {txs.is_empty().then(|| view! { <div class="small cap">"No transaction yet: a transacted session or an XA resource opens one."</div> })} }.into_any()
                    } else {
                        view! {
                            <div class="row" style="gap:40px">{move || { let c = dest()["counts"].clone(); [("scheduled", "scheduled", ""), ("pending", "pending", "attn"), ("delivered", "delivered", ""), ("acked", "acked", ""), ("dead_letter", "dead-letter", "danger"), ("expired", "expired", "danger")].into_iter().map(|(k, l, cl)| view! {
                                <div><div class=cl style="font-size:22px;font-weight:500">{n(&c, k).to_string()}</div><div class="small cap">{l}</div></div> }).collect_view() }}</div>
                            <table class="tbl"><tr><th>"Seq"</th><th>"Text"</th><th>"State"</th><th>"Redelivered"</th><th>"By"</th><th>"When"</th></tr>
                                {msgs.get().into_iter().map(|m| { let state = s(&m, "state"); let cl = match state.as_str() { "pending" => "attn", "acked" => "ok", "dead_letter" | "expired" => "danger", _ => "" };
                                    // A message inside an open transaction says so.
                                    let state = if m["transaction"].is_null() { state } else { format!("{state} · in tx") };
                                    // Moved to a dead-letter queue: where; a repeating or cron schedule: which.
                                    let state = match (m["movedTo"].is_null(), m["schedule"]["cron"].as_str().filter(|c| !c.is_empty())) {
                                        (false, _) => format!("{state} → {} #{}", s(&m["movedTo"], "destination").trim_start_matches("queue://"), n(&m["movedTo"], "seq")),
                                        (true, Some(c)) => format!("{state} · cron {c}"),
                                        (true, None) if !m["schedule"].is_null() => format!("{state} · repeats {}", n(&m["schedule"], "left")),
                                        _ => state,
                                    };
                                    let body = if m["text"].is_null() { format!("{} bytes (base64)", s(&m, "bodyBase64").len() * 3 / 4) } else { s(&m, "text") };
                                    // A consumer id carries the client's host name: the app, with the id on hover.
                                    let (who, full) = if s(&m, "consumer").is_empty() { (s(&m, "sender"), String::new()) } else { ("app".to_string(), s(&m, "consumer")) };
                                    view! { <tr><td class="mono">{format!("#{}", n(&m, "seq"))}</td><td class="mono small" title=body.clone()>{body.chars().take(120).collect::<String>()}</td>
                                    <td class=cl>{state}</td><td class="mono">{n(&m, "redeliveryCount").to_string()}</td><td title=full>{who}</td><td class="mono">{api::hms(n(&m, "enqueuedAt"))}</td></tr> } }).collect_view()}</table>
                            {msgs.get().is_empty().then(|| view! { <div class="small cap">"No message yet."</div> })}
                            <div class="caps" style="margin-top:12px">"Send a message · as the partner would"</div>
                            <div class="stack" style="max-width:640px">
                                <textarea class="input mono" rows="2" placeholder="{\"position\": \"POS-1043\", \"lat\": 40.72, \"lon\": -74.0}" prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))></textarea>
                                <div class="grid2"><input class="input mono" placeholder="properties {\"region\": \"north\"}" prop:value=move || props.get() on:input=move |e| props.set(event_target_value(&e))/>
                                    <input class="input mono" placeholder="correlation id" prop:value=move || corr.get() on:input=move |e| corr.set(event_target_value(&e))/></div>
                                <div><button class="btn primary small" on:click=send disabled=move || cur.get().is_empty()>"Send"</button></div></div>
                        }.into_any()
                    }}
                    <div class="caps" style="margin-top:14px">"Provoke"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">
                        <button class="btn small" class:primary=move || st()["refuseLogin"].as_bool().unwrap_or(false) on:click=move |_| set(json!({"refuse_login": true}), "Logins refused")>"Refuse login"</button>
                        <button class="btn small" class:primary=move || { n(&st(), "dropAfter") > 0 } on:click=move |_| set(json!({"drop_after": 3}), "Drops after 3 dispatches")>"Drop after 3"</button>
                        <button class="btn small" class:primary=move || { n(&st(), "slowDispatchMs") > 0 } on:click=move |_| set(json!({"slow_dispatch": "2s"}), "Slow dispatch")>"Slow dispatch (2 s)"</button>
                        <button class="btn small" class:primary=move || { n(&st(), "redeliver") > 0 } on:click=move |_| set(json!({"redeliver": 2}), "The next 2 acks are lost")>"Redeliver next 2"</button>
                        <button class="btn small" class:primary=move || { n(&st(), "failCommits") > 0 } on:click=move |_| set(json!({"fail_commits": 1}), "The next commit fails")>"Fail next commit"</button>
                        <button class="btn small" class:primary=move || { let x = st(); !x.is_null() && !x["refuseLogin"].as_bool().unwrap_or(false) && n(&x, "dropAfter") + n(&x, "slowDispatchMs") + n(&x, "redeliver") + n(&x, "failCommits") == 0 }
                            on:click=move |_| set(json!({"refuse_login": false, "drop_after": 0, "slow_dispatch": "0", "redeliver": 0, "fail_commits": 0}), "Back to normal")>"Normal"</button>
                    </div>
                    <div class="caps" style="margin-top:14px">"Users"</div>
                    <div class="row" style="gap:8px;max-width:640px"><input class="input mono" style="width:316px" placeholder=move || { let u = st()["users"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default(); if u.is_empty() { "acme:s3cret, globex:pw".to_string() } else { format!("now: {u} (type name:password to replace)") } }
                        prop:value=move || users.get() on:input=move |e| users.set(event_target_value(&e))/>
                        <button class="btn small" on:click=move |_| set(json!({"users": users.get_untracked().replace(' ', "")}), "Users saved")>"Save users"</button></div>
                    <div class="small cap">{move || if st()["anyUser"].as_bool().unwrap_or(true) { "Empty: any user name and password log in.".to_string() } else { "Only these users log in. Save an empty list to let anyone in.".to_string() }}</div>
                    <div class="mono small cap">{move || format!("brokerURL tcp://localhost:{0}   ·   OpenWire {1}   ·   failover:(tcp://localhost:{0}) works too", n(&data.get(), "port"), n(&data.get(), "openwireVersion"))}</div>
                    <div class="mono small cap">{move || format!("dead letters → {}   ·   after {} redeliveries   ·   expired persistent messages too", s(&st(), "deadLetterQueue").replace("queue://", ""), n(&st(), "maxRedeliveries"))}</div>
                </div>
            </div>
        </div>
    }
}
