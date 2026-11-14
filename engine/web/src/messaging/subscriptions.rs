//! M1 v2 · Subscriptions: a topic's subscriptions (RabbitMQ: an exchange's bound queues) as a
//! table: what waits, locked, dead-letter, who receives, last activity. Subscriptions live here,
//! under their topic, so the column shows one name per row.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::Value;

use super::{holders, href, parent_topic, Sel};
use crate::api::{self, enc, s};

#[component]
pub fn Subscriptions() -> impl IntoView {
    let sel = expect_context::<Sel>();
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let consumers = RwSignal::new(Vec::<Value>::new());
    let last = RwSignal::new(std::collections::HashMap::<String, i64>::new());
    Effect::new(move |_| {
        live.tick.track();
        let (ns, e) = (sel.ns.get(), sel.entity.get());
        if ns.is_empty() || e.is_empty() { return; }
        let rabbit = s(&sel.namespace_untracked(), "kind") == "rabbitmq";
        leptos::task::spawn_local(async move {
            // RabbitMQ consumers sit on the bound queues, not under the exchange: ask for all.
            let who = if rabbit { String::new() } else { e.clone() };
            if let Ok(v) = api::get(&format!("/messaging/{}/{}/consumers", enc(&ns), enc(if who.is_empty() { "*" } else { &who }))).await { consumers.set(v["consumers"].as_array().cloned().unwrap_or_default()); }
            if let Ok(v) = api::get(&format!("/messaging/{}/{}/messages?last=300", enc(&ns), enc(&e))).await {
                let mut m = std::collections::HashMap::new();
                for x in v["messages"].as_array().cloned().unwrap_or_default() {
                    let at = api::n(&x, "settledAt").max(api::n(&x, "enqueuedAt"));
                    let k = s(&x, "entity");
                    let cur = m.entry(k).or_insert(0);
                    if at > *cur { *cur = at; }
                }
                last.set(m);
            }
        });
    });
    view! {
        <div class="body"><div class="pane">
            {move || {
                let x = sel.namespace();
                let topic = sel.entity.get();
                let rabbit = s(&x, "kind") == "rabbitmq";
                let subs: Vec<(String, String, Value, Value)> = holders(&x).into_iter().filter(|h| h.2 == "subscription" && parent_topic(&x, &h.0).as_deref() == Some(topic.as_str()))
                    .map(|h| { let cfg = x["entities"].as_array().and_then(|a| a.iter().find(|t| s(t, "name") == topic)).and_then(|t| t["subscriptions"].as_array().and_then(|ss| ss.iter().find(|q| s(q, "holder") == h.0 || s(q, "name") == h.0).cloned())).unwrap_or(Value::Null); (h.0, h.1, h.3, cfg) }).collect();
                let cons = consumers.get();
                let receivers = |holder: &str| cons.iter().filter(|c| c["state"] == "receiving" && s(c, "entity") == holder).count();
                let receiving = subs.iter().filter(|h| receivers(&h.0) > 0).count();
                let lm = last.get();
                let newest = lm.values().max().copied();
                let first = subs.first().map(|h| h.3.clone()).unwrap_or(Value::Null);
                let nav = nav.clone();
                if subs.is_empty() {
                    return view! { <div class="callout" style="margin:24px"><div class="t">{format!("{topic} has no {} yet", if rabbit { "bound queues" } else { "subscriptions" })}</div>
                        <div class="t2">{if rabbit { "Messages published here go nowhere until a queue is bound. The app binds one when it subscribes." } else { "Messages sent here are dropped until a subscription exists. The app (or the admin client) makes them; or add one in Settings." }}</div></div> }.into_any();
                }
                view! {
                    <div class="small" style="padding:18px 24px 14px;color:var(--text-2)">{format!("{} {}  ·  {receiving} receiving{}{}",
                        subs.len(), if rabbit { if subs.len() == 1 { "bound queue" } else { "bound queues" } } else if subs.len() == 1 { "subscription" } else { "subscriptions" },
                        newest.map(|t| format!("  ·  last message {}", api::hm(t))).unwrap_or_default(),
                        if rabbit { String::new() } else { format!("  ·  lock {}  ·  max delivery {}", s(&first, "lock"), api::n(&first, "maxDelivery")) })}</div>
                    <div class="mtable">
                        <div class="mhead"><span class="grow">{if rabbit { "BOUND QUEUE" } else { "SUBSCRIPTION" }}</span><span style="width:70px">"WAITING"</span><span style="width:70px">"LOCKED"</span><span style="width:60px">"DL"</span><span style="width:90px">"RECEIVERS"</span><span style="width:150px">"LAST"</span></div>
                        {subs.into_iter().map(|(holder, short, c, cfg)| {
                            let w = ["active", "scheduled"].iter().map(|k| c[*k].as_i64().unwrap_or(0)).sum::<i64>();
                            let lk = c["locked"].as_i64().unwrap_or(0);
                            let dl = c["dead_letter"].as_i64().unwrap_or(0);
                            let r = receivers(&holder);
                            let starving = w > 0 && r == 0;
                            let f = s(&cfg, "filter");
                            let filt = if f.is_empty() || f == "#" { "every message".to_string() } else if rabbit { format!("binding key {f}") } else { format!("filter {f}") };
                            let l = lm.get(&holder).copied();
                            let nav = nav.clone();
                            let h2 = holder.clone();
                            view! {
                                <div class="mrow" style=if starving { "box-shadow:inset 3px 0 0 #a8740a" } else { "" } on:click=move |_| nav(&href("messages", &sel.ns.get_untracked(), &h2, ""), Default::default())>
                                    <span class="grow" style="min-width:0"><div class="mono" style="overflow-wrap:anywhere">{short}</div>
                                        <div class="small" class:attn=starving class:cap=!starving>{if starving { format!("{filt}  ·  {w} waiting and nobody receives: no receiver attached") } else { filt }}</div></span>
                                    <span class="mono small" style="width:70px" class:attn={w > 0} class:cap={w == 0}>{w}</span>
                                    <span class="mono small cap" style="width:70px">{lk}</span>
                                    <span class="mono small" style="width:60px" class:danger={dl > 0} class:cap={dl == 0}>{dl}</span>
                                    <span class="small" style="width:90px" class:attn={r == 0} class:ok={r > 0}>{if r == 0 { "none".to_string() } else { r.to_string() }}</span>
                                    <span class="small" style="width:150px;color:var(--text-2)">{match (l, starving) { (Some(t), true) => format!("waiting since {}", api::hm(t)), (Some(t), false) => format!("active {}", api::hm(t)), _ => "—".into() }}</span>
                                </div>
                            }
                        }).collect_view()}
                    </div>
                    <div class="small cap" style="padding:14px 24px;max-width:720px">{if rabbit { "The column lists exchanges and the queues no exchange routes to. Bound queues live here, under their exchange: click one for its messages, dead-letter and consumers." } else { "The column lists topics and queues, one name each. Subscriptions live here, under their topic: click one for its messages, dead-letter and consumers." }}</div>
                }.into_any()
            }}
        </div></div>
    }
}
