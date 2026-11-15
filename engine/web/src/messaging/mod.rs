//! Messaging (M1–M3) in shell B: the rail's "Queues" opens this area. Its own column lists the
//! namespaces (Azure Service Bus, RabbitMQ) with their queues, topics and subscriptions, "+ New"
//! on top and the connection string at the bottom; the tabs act on the namespace and entity
//! picked there: Messages · Dead-letter · Consumers · Rules · Settings.

pub mod activemq;
pub mod consumers;
pub mod eventhubs;
pub mod messages;
pub mod rules;
pub mod settings;
pub mod subscriptions;

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui::{self, Dialog, Pop},
};

/// What the area shares with its tabs.
#[derive(Clone, Copy)]
pub struct Sel {
    pub data: RwSignal<Value>,
    pub ns: Memo<String>,
    pub entity: Memo<String>,
}

impl Sel {
    pub fn namespace(&self) -> Value {
        let n = self.ns.get();
        self.data.get()["namespaces"].as_array().and_then(|a| a.iter().find(|x| s(x, "namespace") == n).cloned()).unwrap_or(Value::Null)
    }
    pub fn namespace_untracked(&self) -> Value {
        let n = self.ns.get_untracked();
        self.data.get_untracked()["namespaces"].as_array().and_then(|a| a.iter().find(|x| s(x, "namespace") == n).cloned()).unwrap_or(Value::Null)
    }
}

/// Every place an entity's messages live, as the column lists them: queues, topics, subscriptions.
pub fn holders(ns: &Value) -> Vec<(String, String, String, Value)> {
    let ents = ns["entities"].as_array().cloned().unwrap_or_default();
    let rabbit = s(ns, "kind") == "rabbitmq";
    let queue_counts = |name: &str| ents.iter().find(|e| s(e, "kind") == "queue" && s(e, "name") == name).map(|e| e["counts"].clone()).unwrap_or(Value::Null);
    // RabbitMQ: a bound queue shows under its exchange only, not again on its own.
    let bound: std::collections::HashSet<String> = if rabbit { ents.iter().flat_map(|e| e["subscriptions"].as_array().cloned().unwrap_or_default()).map(|x| s(&x, "name")).collect() } else { Default::default() };
    let mut v = vec![];
    for e in &ents {
        let name = s(e, "name");
        if s(e, "kind") == "queue" {
            if !bound.contains(&name) {
                v.push((name.clone(), name, "queue".into(), e["counts"].clone()));
            }
        } else {
            v.push((name.clone(), name.clone(), "topic".into(), e["counts"].clone()));
            for sub in e["subscriptions"].as_array().cloned().unwrap_or_default() {
                let sn = s(&sub, "name");
                // "attachment-rejected-event.attachment-rejected" under its exchange reads as "attachment-rejected".
                let short = sn.strip_prefix(&format!("{name}.")).map(String::from).unwrap_or(sn.clone());
                let counts = if rabbit { queue_counts(&sn) } else { sub["counts"].clone() };
                v.push((s(&sub, "holder"), short, "subscription".into(), counts));
            }
        }
    }
    v
}

pub fn href(tab: &str, ns: &str, entity: &str, extra: &str) -> String {
    let path = match tab { "messages" | "" => "/queues".to_string(), t => format!("/queues/{t}") };
    format!("{path}?ns={}&entity={}{extra}", enc(ns), enc(entity))
}

pub fn label(kind: &str) -> &'static str {
    if kind == "rabbitmq" { "RabbitMQ" } else { "Azure Service Bus" }
}

pub fn n(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}

/// The topic (or exchange) an entity belongs to: a subscription's topic, a bound queue's exchange.
pub fn parent_topic(ns: &Value, entity: &str) -> Option<String> {
    if let Some((t, _)) = entity.split_once("/subscriptions/") {
        return Some(t.to_string());
    }
    if s(ns, "kind") == "rabbitmq" {
        for e in ns["entities"].as_array().cloned().unwrap_or_default() {
            if e["subscriptions"].as_array().map(|a| a.iter().any(|x| s(x, "name") == entity)).unwrap_or(false) {
                return Some(s(&e, "name"));
            }
        }
    }
    None
}

/// Column rows: topics (exchanges) and the queues no exchange routes to, one name each, with
/// what waits in them and their dead-letter.
fn column_rows(ns: &Value) -> Vec<(String, &'static str, i64, i64)> {
    let mut topics = vec![];
    let mut queues = vec![];
    let hs = holders(ns);
    let wait = |c: &Value| ["active", "scheduled", "locked"].iter().map(|k| c[*k].as_i64().unwrap_or(0)).sum::<i64>();
    for e in ns["entities"].as_array().cloned().unwrap_or_default() {
        let name = s(&e, "name");
        if s(&e, "kind") == "topic" {
            let subs: Vec<&(String, String, String, Value)> = hs.iter().filter(|h| h.2 == "subscription" && parent_topic(ns, &h.0).as_deref() == Some(name.as_str())).collect();
            let w: i64 = subs.iter().map(|h| wait(&h.3)).sum();
            let d: i64 = subs.iter().map(|h| h.3["dead_letter"].as_i64().unwrap_or(0)).sum();
            topics.push((name, "topic", w, d));
        } else if hs.iter().any(|h| h.0 == name && h.2 == "queue") {
            queues.push((name, "queue", wait(&e["counts"]), e["counts"]["dead_letter"].as_i64().unwrap_or(0)));
        }
    }
    topics.extend(queues);
    topics
}

#[component]
pub fn Area(children: ChildrenFn) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let nav = use_navigate();
    let loc = use_location();
    let data = RwSignal::new(Value::Null);
    let load = move || leptos::task::spawn_local(async move { if let Ok(v) = api::get("/messaging/namespaces").await { data.set(v); } });
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && topic.starts_with("messaging") { load(); }
        t
    });
    let ns = Memo::new(move |_| {
        let all = data.get()["namespaces"].as_array().cloned().unwrap_or_default();
        let want = qm.get().get("ns").unwrap_or_default();
        if all.iter().any(|x| s(x, "namespace") == want) { want } else { all.first().map(|x| s(x, "namespace")).unwrap_or_default() }
    });
    let entity = Memo::new(move |_| {
        let n = ns.get();
        let all = data.get()["namespaces"].as_array().cloned().unwrap_or_default();
        let nsv = all.iter().find(|x| s(x, "namespace") == n).cloned().unwrap_or(Value::Null);
        let hs = holders(&nsv);
        let want = qm.get().get("entity").unwrap_or_default();
        let bound_queue = s(&nsv, "kind") == "rabbitmq" && nsv["entities"].as_array().map(|a| a.iter().any(|e| s(e, "name") == want)).unwrap_or(false);
        if hs.iter().any(|h| h.0 == want) || bound_queue { want } else { column_rows(&nsv).first().map(|r| r.0.clone()).unwrap_or_default() }
    });
    let sel = Sel { data, ns, entity };
    provide_context(sel);
    let tab = move || {
        let p = loc.pathname.get();
        p.strip_prefix("/queues/").unwrap_or("messages").split('/').next().unwrap_or("messages").to_string()
    };
    let new_open = RwSignal::new(false);
    let publish_open = RwSignal::new(false);
    provide_context(PublishOpen(publish_open));
    let filter = RwSignal::new(String::new());
    let pick_ns = RwSignal::new(false);
    let n2 = nav.clone();
    view! {
        <div class="area">
            <div class="kcol" style="width:320px">
                <span class="anchor" style="margin:0 20px">
                    <div class="nsbox" on:click=move |_| pick_ns.set(true)>
                        {move || { let x = sel.namespace(); view! {
                            <span class="dot" style=format!("background:{}", if s(&x, "kind") == "rabbitmq" { "#FF6600" } else { "#E8743B" })></span>
                            <span class="grow" style="min-width:0"><div style="font-weight:600">{if x.is_null() { "No namespace".to_string() } else { s(&x, "namespace") }}</div>
                                <div class="small cap">{if x.is_null() { "make one with + New".to_string() } else { format!("{}  ·  {} {}  ·  {} queue{}", label(&s(&x, "kind")), n(&x, "topics"), if s(&x, "kind") == "rabbitmq" { "exchanges" } else { "topics" }, n(&x, "queues"), if n(&x, "queues") == 1 { "" } else { "s" }) }}</div></span>
                            <span class="small cap">"⌄"</span> } }}
                    </div>
                    <ui::Pop open=pick_ns>
                        {move || data.get()["namespaces"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let nm = s(&x, "namespace"); let nav = use_navigate(); let t = tab();
                            view! { <div class="mi" on:click=move |_| nav(&href(&t, &nm, "", ""), Default::default())><span>{s(&x, "namespace")}</span><span class="sub">{label(&s(&x, "kind"))}</span></div> } }).collect_view()}
                        <a class="mi" href="/queues/activemq" style="text-decoration:none;color:inherit"><span>"ActiveMQ"</span><span class="sub">"OpenWire broker · 61616"</span></a>
                        <a class="mi" href="/queues/eventhubs" style="text-decoration:none;color:inherit"><span>"Event Hubs"</span><span class="sub">"Azure · AMQP and Kafka"</span></a>
                        <div class="rule"></div>
                        <div class="mi" on:click=move |_| new_open.set(true)>"+ New namespace"</div>
                    </ui::Pop>
                </span>
                {move || {
                    let all = data.get()["namespaces"].as_array().cloned().unwrap_or_default();
                    let cur = ns.get();
                    let others: Vec<Value> = all.into_iter().filter(|x| s(x, "namespace") != cur).collect();
                    let menu = RwSignal::new(false);
                    let x = sel.namespace();
                    view! {
                        <div class="row small" style="margin:0 20px;gap:6px">
                            {(!others.is_empty()).then(|| { let o = others[0].clone(); let nav = n2.clone(); let t = tab(); let nm = s(&o, "namespace");
                                view! { <button class="link grow" style="text-align:left" on:click=move |_| nav(&href(&t, &nm, "", ""), Default::default())>{format!("{}{} ›", s(&o, "namespace"), if others.len() > 1 { format!(" and {} more", others.len() - 1) } else { format!(" ({})", label(&s(&o, "kind"))) })}</button> } })}
                            {others.is_empty().then(|| view! { <span class="grow"></span> })}
                            <span class="anchor"><button class="link plain" on:click=move |_| menu.set(true)>"⋯"</button>
                                {(!x.is_null()).then(|| view! { <NsMenu open=menu ns=s(&x, "namespace") kind=s(&x, "kind") connection=s(&x, "connection")/> })}</span>
                        </div>
                        {(!["ok", ""].contains(&s(&x["behaviour"], "reachability").as_str()) && !x.is_null()).then(|| view! { <div class="small attn" style="margin:0 20px">{format!("unreachable on purpose: {}", s(&x["behaviour"], "reachability"))}</div> })}
                    }
                }}
                <input class="input" placeholder="Filter topics and queues" prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e).to_lowercase())/>
                <div class="krows" style="margin-top:2px">
                    {move || {
                        let x = sel.namespace();
                        if x.is_null() {
                            return view! { <div class="small cap" style="padding:8px 20px">"No namespace yet. Point an app at Stuntman (port 5673) and it appears here, or make one."</div> }.into_any();
                        }
                        let rabbit = s(&x, "kind") == "rabbitmq";
                        let f = filter.get();
                        let rows: Vec<_> = column_rows(&x).into_iter().filter(|r| f.is_empty() || r.0.to_lowercase().contains(&f)).collect();
                        let cur = entity.get();
                        let cur_top = parent_topic(&x, &cur).unwrap_or(cur.clone());
                        let (tn, qn) = (rows.iter().filter(|r| r.1 == "topic").count(), rows.iter().filter(|r| r.1 == "queue").count());
                        let nav = nav.clone();
                        let head = move |t: &'static str| view! { <div class="row" style="padding:10px 20px 4px"><span class="caps grow">{t}</span><span class="caps">"Waiting · DL"</span></div> };
                        view! {
                            {(tn > 0).then(|| head(if rabbit { "Exchanges" } else { "Topics" }))}
                            {rows.iter().filter(|r| r.1 == "topic").cloned().map(|r| row_view(r, &cur_top, &nav, sel)).collect_view()}
                            {(qn > 0).then(|| head("Queues"))}
                            {rows.iter().filter(|r| r.1 == "queue").cloned().map(|r| row_view(r, &cur_top, &nav, sel)).collect_view()}
                            {(rows.is_empty()).then(|| view! { <div class="small cap" style="padding:8px 20px">{if f.is_empty() { "No topics or queues yet: the app makes them when it connects (or + Add queue or topic in Settings).".to_string() } else { format!("Nothing called “{f}”") }}</div> })}
                        }.into_any()
                    }}
                </div>
                <div style="border-top:1px solid var(--rule);padding:14px 20px 18px">
                    <div class="caps">"Connection"</div>
                    {move || { let x = sel.namespace(); let c = s(&x, "connection"); let c2 = c.clone();
                        let shown = if s(&x, "kind") == "rabbitmq" { c.split('@').nth(1).map(|h| format!("amqp://…@{h}")).unwrap_or(c.clone()) } else { c.split(';').next().unwrap_or("").trim_start_matches("Endpoint=").to_string() };
                        view! {
                            <div class="mono small ell" style="margin-top:8px" title=c.clone()>{shown}</div>
                            <div class="small cap">{if s(&x, "kind") == "rabbitmq" { "vhost = namespace; any user and password work." } else { "Same format as Azure; any key works." }}</div>
                            <button class="link" style="margin-top:6px" on:click=move |_| ui::copy(&c2, "Connection string")>"Copy connection string"</button>
                            {move || data.get()["namespaces"][0]["bindError"].as_str().map(|e| view! { <div class="err" style="margin-top:6px">{format!("AMQP {e}")}</div> })}
                        } }}
                </div>
            </div>
            <div class="page">
                <Head tab=Signal::derive(tab)/>
                {children()}
            </div>
            <NewNamespace open=new_open/>
            <Publish open=publish_open/>
        </div>
    }
}

fn row_view(r: (String, &'static str, i64, i64), cur_top: &str, nav: &impl Fn(&str, leptos_router::NavigateOptions) , sel: Sel) -> impl IntoView {
    let _ = nav;
    let (name, kind, w, d) = r;
    let on = cur_top == name;
    let ns = sel.ns.get_untracked();
    let link = href(if kind == "topic" { "subscriptions" } else { "messages" }, &ns, &name, "");
    view! {
        <a class="krow row" class:on=on href=link title=name.clone() style="padding:6px 20px;align-items:flex-start;text-decoration:none;color:inherit">
            <span class="mono grow" style="font-size:12px;line-height:1.4;overflow-wrap:anywhere;min-width:0" class:strong=on>{name.clone()}</span>
            <span class="mono small" style="flex:none;padding-left:10px" class:attn={w > 0} class:cap={w == 0}>{if w == 0 { "—".to_string() } else { w.to_string() }}</span>
            {(d > 0).then(|| view! { <span class="mono small danger" style="flex:none;padding-left:6px">{format!("· {d}")}</span> })}
        </a>
    }
}

#[derive(Clone, Copy)]
pub struct PublishOpen(pub RwSignal<bool>);

#[component]
fn Head(tab: Signal<String>) -> impl IntoView {
    let sel = expect_context::<Sel>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let PublishOpen(publish) = expect_context::<PublishOpen>();
    let rules = RwSignal::new(0i64);
    Effect::new(move |_| {
        let kind = s(&sel.namespace(), "kind");
        if kind.is_empty() { return; }
        leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/services/{kind}/rules")).await { rules.set(v["rules"].as_array().map(|a| a.len() as i64).unwrap_or(0)); } });
    });
    // What the open entity is: a topic (or exchange), a subscription (or bound queue), a queue.
    let what = Memo::new(move |_| {
        let x = sel.namespace();
        let e = sel.entity.get();
        let is_topic = x["entities"].as_array().map(|a| a.iter().any(|t| s(t, "name") == e && s(t, "kind") == "topic")).unwrap_or(false);
        let parent = parent_topic(&x, &e);
        (if is_topic { "topic" } else if parent.is_some() { "subscription" } else { "queue" }, parent)
    });
    let counts = move || {
        let x = sel.namespace();
        let e = sel.entity.get();
        let hs = holders(&x);
        let mut c = json!({});
        for h in hs.iter().filter(|h| h.0 == e || (what.get().0 == "topic" && parent_topic(&x, &h.0).as_deref() == Some(e.as_str()))) {
            if h.2 == "topic" { continue; }
            for k in ["active", "scheduled", "locked", "completed", "dead_letter"] { c[k] = json!(c[k].as_i64().unwrap_or(0) + h.3[k].as_i64().unwrap_or(0)); }
        }
        c
    };
    let nav_crumb = nav.clone();
    let subs_n = move || { let x = sel.namespace(); let e = sel.entity.get(); x["entities"].as_array().and_then(|a| a.iter().find(|t| s(t, "name") == e).map(|t| t["subscriptions"].as_array().map(|s| s.len()).unwrap_or(0))).unwrap_or(0) };
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || { let x = sel.namespace(); format!("Queues  ·  {}  ·  {}  ·  {}", label(&s(&x, "kind")), s(&x, "namespace"), s(&env.get(), "env").to_uppercase()) }}</div>
                    <div class="row" style="gap:10px">
                        {move || { let (k, parent) = what.get(); let nav = nav_crumb.clone();
                            parent.filter(|_| k == "subscription").map(|p| { let p2 = p.clone(); view! { <button class="link" style="font-size:13px" on:click=move |_| nav(&href("subscriptions", &sel.ns.get_untracked(), &p2, ""), Default::default())>{format!("{p}  ›")}</button> } }) }}
                        <h1>{move || { let e = sel.entity.get(); match e.split_once("/subscriptions/") { Some((_, s)) => s.to_string(), None => if e.is_empty() { "Queues".into() } else { e } } }}</h1>
                        {move || { let (k, _) = what.get(); let rabbit = s(&sel.namespace(), "kind") == "rabbitmq";
                            (!sel.entity.get().is_empty()).then(|| view! { <span class="pill">{match (k, rabbit) { ("topic", true) => "exchange", ("subscription", true) => "bound queue", (k, _) => k }}</span> }) }}
                    </div>
                </div>
                <span class="grow"></span>
                {move || (!sel.ns.get().is_empty()).then(|| view! { <button class="btn" on:click=move |_| publish.set(true)>"Publish"</button> })}
            </div>
            <div class="tabs" class:hidden=move || sel.entity.get().is_empty()>
                {move || {
                    let topic = what.get().0 == "topic";
                    let c = counts();
                    let mut tabs: Vec<(&'static str, &'static str, String)> = vec![];
                    if topic { tabs.push(("subscriptions", "Subscriptions", subs_n().to_string())); }
                    tabs.push(("messages", "Messages", ["active", "scheduled", "locked", "completed"].iter().map(|k| c[*k].as_i64().unwrap_or(0)).sum::<i64>().to_string()));
                    tabs.push(("dead-letter", "Dead-letter", c["dead_letter"].as_i64().unwrap_or(0).to_string()));
                    tabs.push(("consumers", "Consumers", String::new()));
                    tabs.push(("rules", "Rules", rules.get().to_string()));
                    tabs.push(("settings", "Settings", String::new()));
                    tabs.into_iter().map(|(key, lbl, n)| {
                        let nav = nav.clone();
                        view! { <div class="tab" class:on=move || tab.get() == key on:click=move |_| nav(&href(key, &sel.ns.get_untracked(), &sel.entity.get_untracked(), ""), Default::default())>
                            {lbl}<span class="n">{n}</span></div> }
                    }).collect_view()
                }}
            </div>
        </div>
    }
}

#[component]
fn NsMenu(open: RwSignal<bool>, ns: String, kind: String, connection: String) -> impl IntoView {
    let nav = use_navigate();
    let confirm = RwSignal::new(false);
    let (n1, n2, n3) = (ns.clone(), ns.clone(), ns.clone());
    let reach = move |mode: &'static str, ns: String| leptos::task::spawn_local(async move {
        match api::send("PUT", &format!("/messaging/{}/reachability", enc(&ns)), json!({"mode": mode})).await {
            Ok(_) => ui::toast(if mode == "ok" { "Reachable again".to_string() } else { format!("{ns}: {}", match mode { "refuse" => "connections refused", "timeout" => "no answer (AMQP timeout)", "401" => "every key rejected (401)", _ => "every send ServerBusy" }) }),
            Err(e) => ui::toast_err(e),
        }
    });
    let k2 = kind.clone();
    view! {
        <Pop open=open right=true>
            <div class="mi" on:click={ let c = connection.clone(); move |_| ui::copy(&c, "Connection string") }>"Copy connection string"</div>
            <div class="mi" on:click={ let nav = nav.clone(); let n1 = n1.clone(); move |_| nav(&href("settings", &n1, "", ""), Default::default()) }>"Namespace settings"</div>
            <div class="rule"></div>
            <div class="mh caps">"Make it unreachable"</div>
            <div class="mi" on:click={ let n = n2.clone(); move |_| reach("refuse", n.clone()) }>"Refuse the connection"</div>
            <div class="mi" on:click={ let n = n2.clone(); move |_| reach("timeout", n.clone()) }>"AMQP timeout"</div>
            <div class="mi" on:click={ let n = n2.clone(); move |_| reach("401", n.clone()) }>"401 · key rejected"</div>
            <div class="mi" on:click={ let n = n2.clone(); move |_| reach("busy", n.clone()) }>"Server busy (throttling)"</div>
            <div class="mi" on:click={ let n = n2.clone(); move |_| reach("ok", n.clone()) }>"Reachable (normal)"</div>
            <div class="rule"></div>
            <div class="mi danger" on:click=move |_| confirm.set(true)>"Remove namespace"</div>
        </Pop>
        <ui::Confirm open=confirm question=Signal::derive({ let n = ns.clone(); move || format!("Remove {n} and its messages?") }) detail=Signal::derive(|| "Apps using its connection string stop working until it exists again.".to_string()) action="Remove"
            on_yes=move |_| { let (n, k) = (n3.clone(), k2.clone()); leptos::task::spawn_local(async move { match api::send("DELETE", &format!("/services/{k}/accounts/{}", enc(&n)), json!({})).await { Ok(_) => ui::toast("Namespace removed"), Err(e) => ui::toast_err(e) } }); }/>
    }
}

#[component]
fn NewNamespace(open: RwSignal<bool>) -> impl IntoView {
    let nav = use_navigate();
    let kind = RwSignal::new("azure-service-bus".to_string());
    let name = RwSignal::new(String::new());
    let err = RwSignal::new(String::new());
    let make = Callback::new(move |_: ()| {
        let (k, nm) = (kind.get_untracked(), name.get_untracked());
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/services/{k}/accounts"), json!({"account": nm})).await {
                Ok(_) => { open.set(false); name.set(String::new()); ui::toast("Namespace ready: copy its connection string"); nav(&href("messages", &nm, "", ""), Default::default()); }
                Err(e) => err.set(e),
            }
        });
    });
    view! {
        <Dialog open=open title="New namespace">
            <div class="field"><label>"Kind"</label>
                <ui::Seg options=vec![("azure-service-bus", "Azure Service Bus"), ("rabbitmq", "RabbitMQ")] value=kind set=Callback::new(move |v| kind.set(v))/></div>
            <div class="field" style="margin-top:14px"><label>"Name"</label>
                <input class="input mono" placeholder="acme-bus-stg" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e).trim().to_lowercase())/>
                <div class="small cap">{move || if kind.get() == "rabbitmq" { "It is the vhost: amqp://…/<name>. Apps can also make one just by connecting." } else { "It is the host: sb://<name>.localhost. Apps can also make one just by connecting." }}</div></div>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
            <div class="row" style="justify-content:flex-end;gap:10px;margin-top:18px">
                <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button>
                <button class="btn primary" disabled=move || name.get().len() < 3 on:click=move |_| make.run(())>"Make it"</button></div>
        </Dialog>
    }
}

/// M1 · Publish form: recorded as sent by you, not by the app.
#[component]
fn Publish(open: RwSignal<bool>) -> impl IntoView {
    let sel = expect_context::<Sel>();
    let to = RwSignal::new(String::new());
    let subject = RwSignal::new(String::new());
    let body = RwSignal::new("{\n  \"orderId\": 58300,\n  \"total\": 75.00\n}".to_string());
    let when = RwSignal::new("now".to_string());
    let props = RwSignal::new(Vec::<(String, String)>::new());
    let err = RwSignal::new(String::new());
    Effect::new(move |_| { if open.get() { let e = sel.entity.get_untracked(); to.set(e.split("/subscriptions/").next().unwrap_or("").to_string()); err.set(String::new()); } });
    let go = move |_| {
        let ns = sel.ns.get_untracked();
        let mut p = serde_json::Map::new();
        for (k, v) in props.get_untracked() { if !k.trim().is_empty() { p.insert(k, json!(v)); } }
        let b = json!({"subject": subject.get_untracked(), "body": body.get_untracked(), "properties": p, "schedule_at": match when.get_untracked().as_str() { "now" => Value::Null, w => json!(w) }});
        let e = to.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/messaging/{}/{}/messages", enc(&ns), enc(&e)), b).await {
                Ok(v) => { open.set(false); ui::toast(format!("Published: {} cop{}", api::n(&v, "copies"), if api::n(&v, "copies") == 1 { "y" } else { "ies" })); }
                Err(x) => err.set(x),
            }
        });
    };
    view! {
        <Dialog open=open title="Publish a message">
            <div class="small cap" style="margin-top:-6px">"Recorded as sent by you, not by the app"</div>
            <table class="kv2" style="margin-top:12px">
                <tr><td>"To"</td><td><select class="select" on:change=move |e| to.set(event_target_value(&e))>
                    {move || { let x = sel.namespace(); x["entities"].as_array().cloned().unwrap_or_default().into_iter().map(|e| { let nm = s(&e, "name"); let n2 = nm.clone(); view! { <option value=nm.clone() selected=move || to.get() == n2>{format!("{nm}{}", if s(&e, "kind") == "topic" { "  (topic)" } else { "" })}</option> } }).collect_view() }}
                </select></td></tr>
                <tr><td>"Subject"</td><td><input class="input mono" style="width:100%" placeholder="OrderCreated" prop:value=move || subject.get() on:input=move |e| subject.set(event_target_value(&e))/></td></tr>
                <tr><td>"Body"</td><td><textarea class="input mono" style="width:100%;height:120px;padding:8px" prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea></td></tr>
                <tr><td>"Deliver"</td><td><select class="select" on:change=move |e| when.set(event_target_value(&e))>
                    {[("now", "Now"), ("30s", "In 30 s"), ("5m", "In 5 min"), ("1h", "In 1 hour")].into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}</select></td></tr>
                <tr><td>"Properties"</td><td>
                    {move || props.get().into_iter().enumerate().map(|(i, (k, v))| view! { <div class="row" style="gap:6px;margin-bottom:6px">
                        <input class="input mono" style="width:120px" placeholder="key" prop:value=k on:input=move |e| props.update(|p| p[i].0 = event_target_value(&e))/>
                        <input class="input mono" style="width:150px" placeholder="value" prop:value=v on:input=move |e| props.update(|p| p[i].1 = event_target_value(&e))/>
                        <button class="link plain" on:click=move |_| props.update(|p| { p.remove(i); })>"×"</button></div> }).collect_view()}
                    <button class="link" on:click=move |_| props.update(|p| p.push((String::new(), String::new())))>"+ Add property"</button></td></tr>
            </table>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
            <div class="row" style="justify-content:flex-end;gap:10px;margin-top:18px">
                <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button>
                <button class="btn primary" disabled=move || to.get().is_empty() on:click=go>"Publish"</button></div>
        </Dialog>
    }
}
