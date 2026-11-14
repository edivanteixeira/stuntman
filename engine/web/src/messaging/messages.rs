//! M1 · Messages and M1b · Dead-letter: counters (a counter is a filter), "Next delivery", the
//! messages newest first, and the open message with its properties, body and delivery history.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{href, Sel};
use crate::{
    api::{self, enc, s},
    ui::{self, Pop},
};

const BEHAVIORS: [(&str, &str); 7] = [("normal", "Normal"), ("lose_lock", "Lose the lock (redeliver)"), ("duplicate", "Deliver twice (duplicate)"), ("hold", "Hold 30 s, then deliver"),
    ("drop", "Drop it silently"), ("dead_letter", "Dead-letter it now"), ("out_of_order", "Deliver out of order (sessions)")];

fn ago(ms: i64) -> String {
    let d = api::now() as i64 - ms;
    if d < 3_600_000 * 20 { api::hm(ms) } else { format!("{} d ago", d / 86_400_000) }
}

fn state_line(m: &Value, next: &Value, first_active: bool) -> (String, &'static str) {
    let st = s(m, "state");
    let dc = api::n(m, "deliveryCount");
    match st.as_str() {
        "completed" => (if dc > 1 { format!("completed  ·  {dc} attempts") } else { "completed".into() }, "cap"),
        "locked" => {
            let left = m["lockedUntil"].as_i64().map(|u| ((u - api::now() as i64) / 1000).max(0));
            (match left { Some(l) => format!("locked by {}  ·  {l} s left", s(m, "lockedBy").split(':').next().unwrap_or("")), None => "delivered, waiting for the ack".into() }, "")
        }
        "scheduled" => (format!("scheduled for {}", api::hm(api::n(m, "availableAt"))), ""),
        "dead_letter" => (format!("dead-lettered  ·  {}", { let r = s(m, "deadLetterReason"); if r.is_empty() { "by hand".to_string() } else { r } }), "danger"),
        _ if first_active && s(next, "behavior") != "normal" => (format!("waiting  ·  next delivery: {}", s(next, "behavior").replace('_', " ")), "attn"),
        _ => ("waiting".into(), ""),
    }
}

#[component]
pub fn Messages(#[prop(optional)] dlq: bool) -> impl IntoView {
    let sel = expect_context::<Sel>();
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let nav = use_navigate();
    let state = RwSignal::new(if dlq { "dead_letter".to_string() } else { "all".to_string() });
    let q = RwSignal::new(String::new());
    let data = RwSignal::new(Value::Null);
    let picked = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let (ns, e, st, qq) = (sel.ns.get(), sel.entity.get(), state.get(), q.get());
        if ns.is_empty() || e.is_empty() { data.set(Value::Null); return; }
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/messaging/{}/{}/messages?state={st}&q={}&last=50", enc(&ns), enc(&e), enc(&qq))).await { data.set(v); }
        });
    });
    let rows = Memo::new(move |_| data.get()["messages"].as_array().cloned().unwrap_or_default());
    // The open message follows ?seq=, else the newest.
    Effect::new(move |_| {
        live.tick.track();
        let seq = qm.get().get("seq").and_then(|x| x.parse::<i64>().ok()).or_else(|| rows.get().first().map(|r| api::n(r, "seq")));
        let ns = sel.ns.get();
        let Some(seq) = seq else { picked.set(Value::Null); return };
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/messaging/{}/x/messages/{seq}", enc(&ns))).await { picked.set(x); } });
    });
    let next = Memo::new(move |_| data.get()["next"].clone());
    let set_next = move |e: leptos::ev::Event| {
        let b = event_target_value(&e);
        let (ns, ent) = (sel.ns.get_untracked(), sel.entity.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/messaging/{}/{}/next-delivery", enc(&ns), enc(&ent)), json!({"behavior": b, "times": 1})).await {
                Ok(_) => ui::toast(if b == "normal" { "Next delivery: normal".to_string() } else { format!("The next message: {}, once", b.replace('_', " ")) }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let purge_open = RwSignal::new(false);
    let super::PublishOpen(publish) = expect_context::<super::PublishOpen>();
    let resubmit_all = move |_| {
        let (ns, ent) = (sel.ns.get_untracked(), sel.entity.get_untracked());
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/messaging/{}/{}/resubmit", enc(&ns), enc(&ent)), json!({"all_dead_letters": true})).await { Ok(v) => ui::toast(format!("{} back on the queue", api::n(&v, "resubmitted"))), Err(e) => ui::toast_err(e) } });
    };
    view! {
        <div class="body">
            <div class="pane">
                {move || {
                    let nav_rows = nav.clone();
                    let d = data.get();
                    let ns = sel.ns.get();
                    if ns.is_empty() {
                        return view! { <div class="callout" style="margin:24px"><div class="t">"No namespace yet"</div>
                            <div class="t2">"Point the app's Service Bus connection string or RabbitMQ URI at Stuntman (port 5673): the namespace and the queues it uses appear here. Or make one with + New."</div></div> }.into_any();
                    }
                    let c = d["counts"].clone();
                    let lim = d["limits"].clone();
                    let e = sel.entity.get();
                    let kind = s(&sel.namespace(), "kind");
                    let path = format!("{ns}  /  {}  /  {}", if e.contains("/subscriptions/") { "topics" } else { "queues" }, e.replace("/subscriptions/", " / "));
                    let total: i64 = ["active", "scheduled", "locked", "completed", "dead_letter"].iter().map(|k| c[*k].as_i64().unwrap_or(0)).sum();
                    let oldest_dl = rows.get().iter().filter(|m| m["state"] == "dead_letter").map(|m| api::n(m, "settledAt").max(api::n(m, "enqueuedAt"))).min();
                    view! {
                        <div style="padding:18px 24px 10px">
                            <div class="row"><span class="mono small cap grow">{if dlq { format!("{path}  /  $DeadLetterQueue") } else { path }}</span>
                                {(!dlq).then(|| view! { <span class="small cap">{format!("lock {} s  ·  max delivery {}", api::n(&lim, "lock"), api::n(&lim, "maxDelivery"))}</span> })}</div>
                            {if dlq {
                                let n_dl = c["dead_letter"].as_i64().unwrap_or(0);
                                view! {
                                    <div class="row" style="margin-top:10px;gap:8px"><span style="font:600 22px var(--sans)" class:danger={n_dl > 0}>{n_dl}</span>
                                        <span>{if n_dl == 1 { "message could not be processed" } else { "messages could not be processed" }}</span>
                                        {oldest_dl.map(|o| view! { <span class="small cap">{format!("·  the oldest is {}", api::span(api::now() as i64 - o))}</span> })}</div>
                                    <div class="row" style="gap:12px;margin-top:10px">
                                        <button class="btn" disabled={n_dl == 0} on:click=resubmit_all>{format!("Resubmit all {n_dl}")}</button>
                                        <button class="link danger" disabled={n_dl == 0} on:click=move |_| purge_open.set(true)>"Purge all"</button></div>
                                }.into_any()
                            } else {
                                view! {
                                    <div class="row" style="gap:36px;margin-top:12px">
                                        {[("active", "active", ""), ("scheduled", "scheduled", ""), ("locked", "locked", ""), ("dead_letter", "dead-letter", "danger")].into_iter().map(|(k, l, tone)| {
                                            let v = c[k].as_i64().unwrap_or(0);
                                            view! { <div style="cursor:pointer;padding-bottom:4px" class:ctr-on=move || state.get() == k on:click=move |_| state.update(|x| *x = if x == k { "all".into() } else { k.into() })>
                                                <div style="font:600 20px var(--sans)" class:danger={tone == "danger" && v > 0}>{v}</div><div class="small cap">{l}</div></div> }
                                        }).collect_view()}
                                    </div>
                                    {move || (state.get() != "all").then(|| view! { <div class="small cap" style="margin-top:6px">{format!("Showing {} only  ·  ", state.get().replace('_', "-"))}<button class="link" on:click=move |_| state.set("all".into())>"Show all"</button></div> })}
                                    <div class="row" style="gap:12px;margin-top:14px">
                                        <span>"Next delivery"</span>
                                        <select class="select" on:change=set_next>
                                            {BEHAVIORS.into_iter().map(|(v, l)| { let on = s(&next.get(), "behavior") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                        <span class="small cap">"once, then normal  ·  "</span>
                                        <a class="link" href={ let b = s(&next.get(), "behavior"); href("rules", &ns, &e, &format!("&new={}", if b == "normal" { "lose_lock".into() } else { b })) }>"Make it a rule ›"</a>
                                        <span class="grow"></span>
                                        <input class="input" style="width:170px" placeholder="Message id, subject…" prop:value=move || q.get() on:input=move |ev| q.set(event_target_value(&ev))/>
                                    </div>
                                    {(kind == "rabbitmq").then(|| view! { <div class="small cap" style="margin-top:8px">"RabbitMQ has no locks: “locked” is a message delivered and not acked yet."</div> })}
                                }.into_any()
                            }}
                        </div>
                        {(total == 0 && !dlq).then(|| { let e2 = e.clone(); view! { <div class="callout" style="margin:8px 24px"><div class="t">{format!("{e2} is empty")}</div>
                            <div class="t2">"Nothing was published here. Wait for the app, or publish a test message to see how it is handled."</div>
                            <button class="btn" on:click=move |_| publish.set(true)>"Publish a test message"</button></div> } })}
                        <div class="mtable">
                            <div class="mhead"><span style="width:70px">"SEQ"</span><span class="grow">"MESSAGE"</span><span style="width:250px">{if dlq { "REASON" } else { "STATE" }}</span><span style="width:60px;text-align:right">"WHEN ↓"</span></div>
                            {move || {
                                let nx = next.get();
                                let rs = rows.get();
                                let first_active = rs.iter().filter(|m| m["state"] == "active").map(|m| api::n(m, "seq")).min();
                                rs.into_iter().map(|m| {
                                    let seq = api::n(&m, "seq");
                                    let (line, tone) = if dlq { (s(&m, "deadLetterReason"), "danger") } else { state_line(&m, &nx, Some(seq) == first_active) };
                                    let sub = if dlq { let d = s(&m, "deadLetterDescription"); if d.is_empty() { format!("delivered {} times", api::n(&m, "deliveryCount")) } else { d } } else { String::new() };
                                    let preview = { let b = m["system"]["routingKey"].as_str().unwrap_or(""); let _ = b; format!("{}  ·  from {}", s(&m, "messageId").chars().take(24).collect::<String>(), if s(&m, "sender").is_empty() { "?".into() } else { s(&m, "sender") }) };
                                    let nav = nav_rows.clone();
                                    let (ns2, e2) = (sel.ns.get_untracked(), sel.entity.get_untracked());
                                    view! {
                                        <div class="mrow" class:on=move || api::n(&picked.get(), "seq") == seq on:click=move |_| nav(&href(if dlq { "dead-letter" } else { "messages" }, &ns2, &e2, &format!("&seq={seq}")), Default::default())>
                                            <span class="mono small cap" style="width:70px">{format!("#{seq}")}</span>
                                            <span class="grow" style="min-width:0"><div class="mono">{if s(&m, "subject").is_empty() { "(no subject)".to_string() } else { s(&m, "subject") }}</div><div class="small cap ell">{preview}</div></span>
                                            <span style="width:250px" class=format!("small {tone}")><div>{line}</div>{(!sub.is_empty()).then(|| view! { <div class="cap">{sub.clone()}</div> })}</span>
                                            <span class="mono small" style="width:60px;text-align:right">{ago(api::n(&m, "enqueuedAt"))}</span>
                                        </div>
                                    }
                                }).collect_view()
                            }}
                        </div>
                        <div class="small cap" style="padding:14px 24px">{if dlq { format!("Messages stay here until resubmitted or purged. The app can read them too, at {}/$DeadLetterQueue.", e) } else { "Last 50 messages, newest first. Completed messages stay for the namespace's “keep completed” time for inspection.".to_string() }}</div>
                    }.into_any()
                }}
            </div>
            <div class="side">{move || detail(picked.get(), dlq, sel)}</div>
            <ui::Confirm open=purge_open question=Signal::derive(move || format!("Purge every dead-lettered message of {}?", sel.entity.get())) detail=Signal::derive(|| "They are deleted with their history. This cannot be undone.".to_string()) action="Purge"
                on_yes=move |_| { let (ns, e) = (sel.ns.get_untracked(), sel.entity.get_untracked()); leptos::task::spawn_local(async move { match api::send("DELETE", &format!("/messaging/{}/{}/messages?state=dead_letter", enc(&ns), enc(&e)), json!({})).await { Ok(v) => ui::toast(format!("{} purged", api::n(&v, "purged"))), Err(e) => ui::toast_err(e) } }); }/>
        </div>
    }
}

fn detail(m: Value, dlq: bool, sel: Sel) -> AnyView {
    if m.is_null() {
        return view! { <div class="small cap">"Pick a message to see its properties, body and delivery history."</div> }.into_any();
    }
    let seq = api::n(&m, "seq");
    let ns = s(&m, "namespace");
    let entity = s(&m, "entity");
    let menu = RwSignal::new(false);
    let body = s(&m, "body");
    let (ns1, ns2, ns3, e1, e2) = (ns.clone(), ns.clone(), ns.clone(), entity.clone(), entity.clone());
    let m2 = m.clone();
    let resend = move |_| {
        let (ns, e, m) = (ns1.clone(), e1.split("/subscriptions/").next().unwrap_or("").to_string(), m2.clone());
        leptos::task::spawn_local(async move {
            let b = json!({"subject": m["subject"], "body": m["body"], "properties": m["properties"], "correlation_id": m["system"]["correlationId"]});
            match api::send("POST", &format!("/messaging/{}/{}/messages", enc(&ns), enc(&e)), b).await { Ok(_) => ui::toast("Sent again as a new message (by you)"), Err(e) => ui::toast_err(e) }
        });
    };
    let trace = s(&m, "traceId");
    let hist = m["history"].as_array().cloned().unwrap_or_default();
    let attempts = hist.iter().filter(|h| s(h, "what").starts_with("Delivered")).count();
    let ttl = m["expiresAt"].as_i64().map(|e| api::span(e - api::n(&m, "enqueuedAt"))).unwrap_or_else(|| "never".into());
    let waiting = s(&m, "state") != "completed" && s(&m, "state") != "dead_letter";
    let provoked = hist.iter().any(|h| s(h, "detail").contains("rule “") || s(h, "detail").contains("set by") || s(h, "what").contains("as you set"));
    view! {
        <div class="row" style="gap:10px"><span class="mono" style="font:500 18px var(--mono)">{format!("#{seq}  ·  {}", if s(&m, "subject").is_empty() { "(no subject)".into() } else { s(&m, "subject") })}</span></div>
        <div class="small cap">{format!("{}  ·  {}  ·  {}", if dlq { format!("{entity}/$DeadLetterQueue") } else { entity.clone() }, api::kb(api::n(&m, "size")), if s(&m, "contentType").is_empty() { "no content type".into() } else { s(&m, "contentType") })}</div>
        <div class="row" style="gap:8px;margin-top:12px">
            {if dlq {
                let n3 = ns2.clone();
                view! { <button class="btn" on:click=move |_| { let ns = n3.clone(); leptos::task::spawn_local(async move { match api::send("POST", &format!("/messaging/{}/x/resubmit", enc(&ns)), json!({"seq": seq})).await { Ok(_) => ui::toast("Resubmitted"), Err(e) => ui::toast_err(e) } }); }>"Resubmit"</button> }.into_any()
            } else { view! { <button class="btn" on:click=resend>"Resend"</button> }.into_any() }}
            <button class="btn" on:click={ let mm = m.clone(); move |_| ui::copy(&serde_json::to_string_pretty(&mm).unwrap_or_default(), "Message") }>"Copy as JSON"</button>
            <span class="anchor"><button class="btn icon" on:click=move |_| menu.set(true)>"⋯"</button>
                <Pop open=menu right=true>
                    {(!dlq && waiting).then(|| { let n = ns3.clone(); view! { <div class="mi" on:click=move |_| { let ns = n.clone(); leptos::task::spawn_local(async move { match api::send("POST", &format!("/messaging/{}/x/messages/{seq}/dead-letter", enc(&ns)), json!({})).await { Ok(_) => ui::toast("Moved to dead-letter"), Err(e) => ui::toast_err(e) } }); }>"Move to dead-letter"</div> } })}
                    <div class="mi" on:click={ let b = body.clone(); move |_| ui::copy(&b, "Body") }>"Copy the body"</div>
                    <div class="rule"></div>
                    <div class="mi danger" on:click={ let n = ns2.clone(); move |_| { let ns = n.clone(); leptos::task::spawn_local(async move { match api::send("DELETE", &format!("/messaging/{}/x/messages/{seq}", enc(&ns)), json!({})).await { Ok(_) => ui::toast("Message deleted"), Err(e) => ui::toast_err(e) } }); } }>"Delete"</div>
                </Pop></span>
        </div>
        {if dlq {
            view! {
                <div class="caps" style="margin-top:22px">"Dead-letter"</div>
                <table class="kv2" style="margin-top:8px">
                    <tr><td>"Reason"</td><td class="mono danger">{s(&m, "deadLetterReason")}</td></tr>
                    <tr><td>"Description"</td><td class="mono">{s(&m, "deadLetterDescription")}</td></tr>
                    <tr><td>"Delivery count"</td><td class="mono">{api::n(&m, "deliveryCount")}</td></tr>
                    <tr><td>"Dead-lettered"</td><td class="mono">{m["settledAt"].as_i64().map(api::hms).unwrap_or_default()}</td></tr>
                </table>
            }.into_any()
        } else {
            view! {
                <div class="caps" style="margin-top:22px">"Properties"</div>
                <table class="kv2" style="margin-top:8px">
                    <tr><td>"MessageId"</td><td class="mono">{s(&m, "messageId")}</td></tr>
                    <tr><td>"CorrelationId"</td><td class="mono">{s(&m["system"], "correlationId")}{(!trace.is_empty()).then(|| view! { "  ·  "<a class="link" href=format!("/telemetry/traces/{trace}")>"trace ↗"</a> })}</td></tr>
                    <tr><td>"SessionId"</td><td class="mono">{let v = s(&m["system"], "sessionId"); if v.is_empty() { "—".into() } else { v }}</td></tr>
                    <tr><td>"Subject"</td><td class="mono">{s(&m, "subject")}</td></tr>
                    <tr><td>"Time to live"</td><td class="mono">{ttl}</td></tr>
                    <tr><td>"Delivery count"</td><td class="mono">{api::n(&m, "deliveryCount")}</td></tr>
                    {m["properties"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| view! { <tr><td>{k}</td><td class="mono">{v.as_str().map(String::from).unwrap_or(v.to_string())}</td></tr> }).collect_view()}
                    {m["system"].get("routingKey").map(|_| view! { <tr><td>"Exchange · key"</td><td class="mono">{format!("{} · {}", s(&m["system"], "exchange"), s(&m["system"], "routingKey"))}</td></tr> })}
                </table>
            }.into_any()
        }}
        <div class="caps" style="margin-top:22px">"Body"</div>
        <pre class="yaml" style="max-height:220px;overflow:auto;white-space:pre-wrap;word-break:break-all;margin-top:8px">{s(&m, "body")}</pre>
        <div class="caps" style="margin-top:18px">{if dlq { "Attempts" } else { "Delivery" }}</div>
        <div style="margin-top:8px">{{
            // Long runs of the same attempt fold: the first two, "… N more", the last two.
            let len = hist.len();
            let fold = dlq && len > 8;
            let more = if fold { len - 6 } else { 0 };
            let rows: Vec<Value> = if fold { hist.iter().take(4).cloned().chain(std::iter::once(json!({"fold": more}))).chain(hist.iter().skip(len - 2).cloned()).collect() } else { hist.clone() };
            rows }.into_iter().map(|h| if h.get("fold").is_some() { view! { <div class="small cap" style="padding:4px 0 4px 78px">{format!("… {} more steps, the same", api::n(&h, "fold"))}</div> }.into_any() } else { view! {
            <div class="row" style="gap:14px;align-items:baseline;padding:4px 0"><span class="mono small cap" style="width:64px;flex:none">{api::hms(api::n(&h, "at"))}</span>
                <span><div>{s(&h, "what")}</div><div class=format!("small {}", match s(&h, "tone").as_str() { "ok" => "ok", "attn" => "attn", "danger" => "danger", _ => "cap" })>{s(&h, "detail")}</div></span></div> }.into_any() }).collect_view()}</div>
        {(dlq && !provoked && attempts > 1).then(|| view! { <div class="small cap" style="margin-top:12px">"No Stuntman rule was on: the handler failed every time, so this failure is the app's own."</div> })}
        {(provoked).then(|| view! { <div class="small attn" style="margin-top:12px">"Provoked: a rule or “Next delivery” shaped this message's path."</div> })}
        {(!trace.is_empty()).then(|| view! { <a class="link" style="display:block;margin-top:10px" href=format!("/telemetry/traces/{trace}")>"Open the trace ›"</a> })}
        {let _ = (e2, sel); ()}
    }.into_any()
}
