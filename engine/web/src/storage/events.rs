//! S4 · Files › Events: subscriptions on the left, every event with where it went and the answer,
//! the event on the right with its payload in the provider's schema, attempts and its cause.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::FilesHead;
use crate::{
    api::{self, enc, s},
    ui::{self, Pop},
};

fn result(e: &Value) -> (String, &'static str) {
    let last = e["lastStatus"].as_i64().map(|x| x.to_string()).unwrap_or_default();
    let n = api::n(e, "attempts");
    match s(e, "status").as_str() {
        "delivered" => (format!("{last}  ·  {}", if n > 1 { format!("{n} attempts") } else { format!("{} ms", api::n(e, "lastMs")) }), ""),
        "retrying" => (format!("{}  ·  attempt {}", if last.is_empty() { "no answer".into() } else { last }, n), "attn"),
        "given_up" => (format!("given up  ·  {}", api::span(api::n(e, "at") - api::now() as i64)), "danger"),
        "dropped" => ("dropped by you".into(), "attn"),
        "held" => ("held: goes after the next one".into(), "attn"),
        "no_destination" => ("waiting for the Queues service".into(), "cap"),
        _ if s(e, "by").starts_with("sent by") => ("sent by you".into(), "attn"),
        _ => ("pending".into(), "cap"),
    }
}

#[component]
pub fn Events() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let qm = use_query_map();
    let q = RwSignal::new(String::new());
    let subs = RwSignal::new(Vec::<Value>::new());
    let evs = RwSignal::new(Vec::<Value>::new());
    let ev = RwSignal::new(Value::Null);
    let sub = RwSignal::new(String::new());
    let send_open = RwSignal::new(false);
    let new_open = RwSignal::new(false);
    let sel = Memo::new(move |_| qm.get().get("id").unwrap_or_default());
    let load = move || leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/storage/subscriptions").await { subs.set(v["subscriptions"].as_array().cloned().unwrap_or_default()); }
        let f = sub.get_untracked();
        let path = if f.is_empty() { "/storage/events?last=200".to_string() } else { format!("/storage/events?last=200&subscription={}", enc(&f)) };
        if let Ok(v) = api::get(&path).await { evs.set(v["events"].as_array().cloned().unwrap_or_default()); }
        let id = sel.get_untracked();
        if !id.is_empty() { ev.set(api::get(&format!("/storage/events/{}", enc(&id))).await.unwrap_or(Value::Null)); }
    });
    load();
    Effect::new(move |_| { sel.track(); sub.track(); load(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("storage.") { load(); }
        t
    });
    let act = move |m: &'static str, path: String, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        match api::send(m, &path, body).await { Ok(_) => ui::toast(done), Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="page">
            <FilesHead active="events" q=q placeholder="Path, event type…" right=move || view! { <button class="btn" on:click=move |_| send_open.set(true)>"Send an event"</button> }/>
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">"Subscriptions"</div>
                    {move || subs.get().into_iter().map(|x| {
                        let name = s(&x, "name");
                        let (n1, n2) = (name.clone(), name.clone());
                        let health = s(&x, "health");
                        let cls = match health.as_str() { "delivering" => "ok", "retrying" => "attn", "not subscribed" => "danger", _ => "cap" };
                        let dest = if s(&x, "to") == "queue" { format!("Service Bus topic  ·  {}", s(&x, "entity")) } else { s(&x, "url") };
                        let evts = x["events"].as_array().map(|e| e.iter().map(|y| { let t = y.as_str().unwrap_or(""); let mut c = t.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                        view! {
                            <div class="subrow" class:on=move || sub.get() == n1 on:click=move |_| sub.set(if sub.get_untracked() == n2 { String::new() } else { n2.clone() })>
                                <div>{name.clone()}</div>
                                <div class="mono small cap ell">{dest}</div>
                                <div class="small t2">{format!("{evts}  ·  {}  ·  {}", if s(&x, "under").is_empty() { "everything".to_string() } else { s(&x, "under") }, if s(&x, "format") == "s3" { "S3 schema" } else { "Event Grid" })}</div>
                                <div class=format!("small {cls}")>{health.clone()}{(api::n(&x, "retrying") > 0).then(|| format!("  ·  {} waiting", api::n(&x, "retrying")))}</div>
                                {(!s(&x, "error").is_empty()).then(|| { let n = name.clone(); view! {
                                    <div class="small danger">{s(&x, "error")}</div>
                                    <button class="link" on:click=move |e| { e.stop_propagation(); act("POST", format!("/storage/subscriptions/{}/validate", enc(&n)), json!({}), "Handshake sent again") }>"Try the handshake again"</button> } })}
                            </div>
                        }
                    }).collect_view()}
                    <button class="link add" on:click=move |_| new_open.set(true)>"+ New subscription"</button>
                    <div class="small cap" style="padding:12px 24px">"Events follow the provider's own format (Event Grid for Azure, S3 notifications for S3), so the app's parser is tested too."</div>
                </div>
                <div class="pane files">
                    <div class="ftable">
                        <div class="fhead caps"><span style="width:200px">"Event"</span><span class="grow">"Sent to"</span><span style="width:120px">"Result"</span><span style="width:56px">"When ↓"</span></div>
                        {move || {
                            let nav = nav.clone();
                            let f = q.get().to_lowercase();
                            evs.get().into_iter().filter(|e| qm.get().get("account").map(|a| s(e, "account") == a).unwrap_or(true)).filter(|e| f.is_empty() || s(e, "path").to_lowercase().contains(&f) || s(e, "type").to_lowercase().contains(&f)).map(|e| {
                                let id = s(&e, "id");
                                let id2 = id.clone();
                                let (r, cls) = result(&e);
                                let nav = nav.clone();
                                view! { <div class="frow" class:on=move || sel.get() == id2 on:click=move |_| nav(&format!("/files/events?id={id}"), Default::default())>
                                    <div style="width:200px"><div class="mono">{s(&e, "short").replace("ObjectCreated:", "ObjectCreated:").to_string()}</div><div class="mono small cap ell">{s(&e, "path")}</div></div>
                                    <span class="grow small">{s(&e, "subscription")}</span>
                                    <span class=format!("small {cls}") style="width:120px">{r}</span>
                                    <span class="mono small" style="width:56px">{if api::now() as i64 - api::n(&e, "at") < 86_400_000 { api::hm(api::n(&e, "at")) } else { crate::environments::day(api::n(&e, "at")) }}</span></div> }
                            }).collect_view()
                        }}
                    </div>
                    {move || (subs.get().is_empty()).then(|| view! {
                        <div class="callout" style="margin:20px 24px"><div class="t">"No one listens yet"</div>
                            <div class="t2">"Events are still recorded here. Add a subscription to deliver them to the app."</div>
                            <button class="btn" on:click=move |_| new_open.set(true)>"New subscription"</button></div> })}
                    <div class="small cap" style="padding:14px 24px">"Retries follow Event Grid: up to 30 attempts in 24 h, then the event is dropped."</div>
                </div>
                <div class="side envside">
                    {move || {
                        let e = ev.get();
                        if e.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick an event to see its payload and every attempt."</div> }.into_any(); }
                        let id = s(&e, "id");
                        let (i1, i2) = (id.clone(), id.clone());
                        let next = RwSignal::new(false);
                        let more = RwSignal::new(false);
                        let sname = s(&e, "subscription");
                        let armed = subs.get().iter().find(|x| s(x, "name") == sname).map(|x| s(x, "nextDelivery")).unwrap_or_default();
                        let payload = serde_json::to_string_pretty(&e["payload"]).unwrap_or_default();
                        let p2 = payload.clone();
                        let cause = e["causedBy"].clone();
                        let open_href = format!("/files?sel={}", enc(&s(&e, "path")));
                        let p3 = payload.clone();
                        view! {
                            <h2 class="h2 mono">{s(&e, "short")}</h2>
                            <div class="small cap">{format!("{}/{}  ·  to {}", s(&e, "container"), s(&e, "path"), sname)}</div>
                            <div class="row" style="margin:12px 0;gap:8px">
                                <button class="btn" on:click=move |_| act("POST", format!("/storage/events/{}/resend", enc(&i1)), json!({}), "Sent again")>"Resend"</button>
                                <button class="btn" on:click=move |_| ui::copy(&p2, "Payload")>"Copy payload"</button>
                                <span class="anchor"><button class="btn icon" on:click=move |_| more.set(true)>"⋯"</button>
                                    <Pop open=more>
                                        <div class="mi" on:click={ let i2 = i2.clone(); move |_| act("POST", format!("/storage/events/{}/resend", enc(&i2)), json!({"to_all": true}), "Sent to every subscription") }>"Send again to every subscription"</div>
                                        <div class="mi" on:click={ let p = p3.clone(); move |_| ui::copy(&format!("curl -X POST -H 'content-type: application/json' -H 'aeg-event-type: Notification' --data '{}' <your endpoint>", p.replace('\'', "'\\''")), "curl") }>"Copy as curl"</div>
                                        <a class="mi" href=open_href.clone()>"Open the object"</a>
                                    </Pop></span>
                            </div>
                            <div class="row" style="gap:12px"><span style="width:100px">"Next delivery"</span>
                                <span class="anchor"><button class="btn" class:armed=!armed.is_empty() on:click=move |_| next.set(true)>{if armed.is_empty() { "Normal".to_string() } else { armed.clone() }}<span class="small">"▾"</span></button>
                                    <Pop open=next>
                                        {[("normal", "Normal"), ("hold:30s", "Hold 30 s, then deliver"), ("duplicate", "Deliver twice"), ("reorder", "Deliver after the next one"), ("drop", "Drop it"), ("missing_field", "Send it with a field missing")]
                                            .into_iter().map(|(v, l)| { let n = sname.clone(); let _ = &n; view! { <div class="mi" on:click=move |_| act("PUT", format!("/storage/subscriptions/{}/next-delivery", enc(&n)), json!({"behavior": v}), "Next delivery set")>{l}</div> } }).collect_view()}
                                        <div class="rule"></div><div class="mh small cap">"To this subscription, once"</div>
                                    </Pop></span>
                                <span class="small cap">"to this subscription, once"</span></div>
                            <div class="caps" style="margin:18px 0 8px">{format!("Payload  ·  {}", if e["payload"]["Records"].is_array() { "S3 notification schema" } else { "Event Grid schema" })}</div>
                            <pre class="yaml envyaml" style="max-height:260px;overflow:auto">{payload}</pre>
                            <div class="caps" style="margin:18px 0 8px">"Delivery"</div>
                            {e["history"].as_array().cloned().unwrap_or_default().into_iter().map(|h| {
                                let st = h["status"].as_i64();
                                let ok = st.map(|x| (200..300).contains(&x)).unwrap_or(false);
                                view! {
                                    <div class="hrow"><span class="mono small cap">{api::hms(api::n(&h, "at"))}</span><div>
                                        <div>{if h["duplicate"] == true { "Sent again (duplicate)".to_string() } else { format!("Sent to {}", s(&h, "url")) }}</div>
                                        <div class="small" class:ok=ok class:attn=!ok>{match st { Some(x) => format!("answered {x} in {} ms{}", api::n(&h, "ms"), if ok { "  ·  the app took it".to_string() } else { h["nextAt"].as_i64().map(|n| format!("  ·  next attempt {}", api::hms(n))).unwrap_or_default() }), None => format!("{}{}", s(&h, "error"), h["nextAt"].as_i64().map(|n| format!("  ·  next attempt {}", api::hms(n))).unwrap_or_default()) }}</div></div></div>
                                }
                            }).collect_view()}
                            {e["history"].as_array().map(|x| x.is_empty()).unwrap_or(true).then(|| view! { <div class="small cap">{result(&e).0}</div> })}
                            {(!cause.is_null()).then(|| view! { <div class="small" style="margin-top:14px"><span class="link">{format!("Caused by: {} by {} at {}", s(&cause, "method"), s(&cause, "caller"), api::hms(api::n(&cause, "at")))}</span></div> })}
                        }.into_any()
                    }}
                </div>
            </div>
            <SendEvent open=send_open subs=subs/>
            <NewSub open=new_open/>
        </div>
    }
}

#[component]
fn SendEvent(open: RwSignal<bool>, subs: RwSignal<Vec<Value>>) -> impl IntoView {
    let to = RwSignal::new(String::new());
    let kind = RwSignal::new("created".to_string());
    let path = RwSignal::new(String::new());
    let send = move |_| {
        let body = json!({"subscription": if to.get_untracked().is_empty() { subs.get_untracked().first().map(|x| s(x, "name")).unwrap_or_default() } else { to.get_untracked() }, "type": kind.get_untracked(), "path": path.get_untracked()});
        leptos::task::spawn_local(async move {
            match api::send("POST", "/storage/events", body).await { Ok(_) => { ui::toast("Sent; recorded as sent by you"); open.set(false); } Err(e) => ui::toast_err(e) }
        });
    };
    view! {
        <ui::Dialog open=open title="Send an event">
            <div class="small cap" style="margin:-10px 0 14px">"Recorded as sent by you; the object is not changed"</div>
            <div class="row" style="margin-bottom:12px"><span style="width:90px">"To"</span>
                <select class="select" on:change=move |e| to.set(event_target_value(&e))>{move || subs.get().into_iter().map(|x| view! { <option value=s(&x, "name")>{s(&x, "name")}</option> }).collect_view()}</select></div>
            <div class="row" style="margin-bottom:12px"><span style="width:90px">"Event"</span>
                <select class="select" on:change=move |e| kind.set(event_target_value(&e))><option value="created">"BlobCreated"</option><option value="deleted">"BlobDeleted"</option></select></div>
            <div class="row"><span style="width:90px">"Object"</span><input class="input mono grow" placeholder="documents/2026/11/…" prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e))/></div>
            <div class="small cap" style="margin:6px 0 0 98px">"Pick an existing object or type any path"</div>
            <div class="actions"><button class="btn" on:click=move |_| open.set(false)>"Cancel"</button><button class="btn primary" disabled=move || !path.get().contains('/') on:click=send>"Send"</button></div>
        </ui::Dialog>
    }
}

#[component]
fn NewSub(open: RwSignal<bool>) -> impl IntoView {
    let to = RwSignal::new("webhook".to_string());
    let url = RwSignal::new(String::new());
    let created = RwSignal::new(true);
    let deleted = RwSignal::new(true);
    let under = RwSignal::new(String::new());
    let format = RwSignal::new("event_grid".to_string());
    let err = RwSignal::new(String::new());
    let save = move |_| {
        let mut ev = vec![];
        if created.get_untracked() { ev.push("created"); }
        if deleted.get_untracked() { ev.push("deleted"); }
        let t = to.get_untracked();
        let body = if t == "queue" { json!({"to": "queue", "entity": url.get_untracked(), "events": ev, "under": under.get_untracked(), "format": format.get_untracked()}) }
                   else { json!({"to": "webhook", "url": url.get_untracked(), "events": ev, "under": under.get_untracked(), "format": format.get_untracked()}) };
        leptos::task::spawn_local(async move {
            match api::send("POST", "/storage/subscriptions", body).await {
                Ok(v) if s(&v, "error").is_empty() => { ui::toast("Subscribed"); open.set(false); err.set(String::new()); }
                Ok(v) => { ui::toast_err(format!("Not subscribed: {}", s(&v, "error"))); open.set(false); }
                Err(e) => err.set(e),
            }
        });
    };
    view! {
        <ui::Dialog open=open title="New subscription">
            <div class="row" style="margin-bottom:12px"><span style="width:90px">"Send to"</span>
                <select class="select" on:change=move |e| to.set(event_target_value(&e))><option value="webhook">"Webhook"</option><option value="queue">"Service Bus topic"</option></select></div>
            <div class="row" style="margin-bottom:12px"><span style="width:90px">{move || if to.get() == "queue" { "Topic" } else { "URL" }}</span>
                <input class="input mono grow" placeholder=move || if to.get() == "queue" { "acme-bus-stg/blob-events" } else { "https://acme-api.stg.example.com/hooks/blob" } prop:value=move || url.get() on:input=move |e| url.set(event_target_value(&e))/></div>
            <div class="row" style="margin-bottom:12px;gap:16px"><span style="width:74px">"Events"</span>
                <label class="check"><input type="checkbox" prop:checked=move || created.get() on:change=move |_| created.update(|v| *v = !*v)/>"Created"</label>
                <label class="check"><input type="checkbox" prop:checked=move || deleted.get() on:change=move |_| deleted.update(|v| *v = !*v)/>"Deleted"</label></div>
            <div class="row" style="margin-bottom:12px"><span style="width:90px">"Only under"</span><input class="input mono grow" placeholder="documents/**" prop:value=move || under.get() on:input=move |e| under.set(event_target_value(&e))/></div>
            <div class="row"><span style="width:90px">"Format"</span>
                <select class="select" on:change=move |e| format.set(event_target_value(&e))><option value="event_grid">"Event Grid"</option><option value="s3">"S3 notification"</option></select></div>
            <div class="small cap" style="margin-top:12px">"Stuntman first sends the validation handshake, like Event Grid. The endpoint must echo the code."</div>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
            <div class="actions"><button class="btn" on:click=move |_| open.set(false)>"Cancel"</button><button class="btn primary" disabled=move || url.get().is_empty() on:click=save>"Subscribe"</button></div>
        </ui::Dialog>
    }
}
