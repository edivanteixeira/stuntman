//! M1c · Consumers: connections receiving from the entity, the locks they hold, what they did
//! lately, and two provocations: Disconnect it, Expire its locks.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{href, Sel};
use crate::{
    api::{self, enc, s},
    ui,
};

#[component]
pub fn Consumers() -> impl IntoView {
    let sel = expect_context::<Sel>();
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let nav = use_navigate();
    let list = RwSignal::new(Vec::<Value>::new());
    let picked = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let (ns, e) = (sel.ns.get(), sel.entity.get());
        if ns.is_empty() { return; }
        leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/messaging/{}/{}/consumers", enc(&ns), enc(&e))).await { list.set(v["consumers"].as_array().cloned().unwrap_or_default()); } });
    });
    Effect::new(move |_| {
        live.tick.track();
        let id = qm.get().get("consumer").or_else(|| list.get().first().map(|c| s(c, "id")));
        let ns = sel.ns.get();
        let Some(id) = id else { picked.set(Value::Null); return };
        leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/messaging/{}/consumers/{}", enc(&ns), enc(&id))).await { picked.set(v); } });
    });
    let act = move |what: &'static str, id: String| {
        let ns = sel.ns.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/messaging/{}/consumers/{}/{what}", enc(&ns), enc(&id)), json!({})).await {
                Ok(v) => ui::toast(if what == "disconnect" { "Disconnected: the app should reconnect".to_string() } else { format!("{} lock{} expired: those messages will be delivered again", api::n(&v, "expired"), if api::n(&v, "expired") == 1 { "" } else { "s" }) }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="body">
            <div class="pane">
                <div style="padding:18px 24px 10px">
                    <div class="mono small cap">{move || format!("{}  /  {}", sel.ns.get(), sel.entity.get().replace("/subscriptions/", " / "))}</div>
                    <div style="font:600 16px var(--sans);margin-top:10px">{move || { let l = list.get(); let r = l.iter().filter(|c| c["state"] == "receiving").count(); let d = l.len() - r;
                        if l.is_empty() { "Nobody is receiving".to_string() } else { format!("{r} receiving{}", if d > 0 { format!("  ·  {d} disconnected") } else { String::new() }) } }}</div>
                </div>
                <div class="mtable">
                    <div class="mhead"><span class="grow">"CONSUMER"</span><span style="width:170px">"STATE"</span><span style="width:70px">"LOCKS"</span><span style="width:90px">"DONE TODAY"</span></div>
                    {move || list.get().into_iter().map(|c| {
                        let id = s(&c, "id");
                        let i2 = id.clone();
                        let nav = nav.clone();
                        let receiving = c["state"] == "receiving";
                        let (ns, e) = (sel.ns.get_untracked(), sel.entity.get_untracked());
                        view! {
                            <div class="mrow" class:on=move || s(&picked.get()["consumer"], "id") == i2 on:click=move |_| nav(&href("consumers", &ns, &e, &format!("&consumer={}", enc(&id))), Default::default())>
                                <span class="grow" style="min-width:0"><div class="mono">{s(&c, "client")}</div>
                                    <div class="small cap ell">{if receiving { format!("prefetch {}  ·  {}", api::n(&c, "prefetch"), s(&c, "mode")) } else { s(&c, "reason") }}</div></span>
                                <span style="width:170px" class=if receiving { "small ok" } else { "small cap" }>{if receiving { "receiving".to_string() } else { format!("disconnected {}", c["until"].as_i64().map(api::hm).unwrap_or_default()) }}</span>
                                <span class="mono small" style="width:70px">{if receiving { api::n(&c, "locks").to_string() } else { "—".into() }}</span>
                                <span class="mono small" style="width:90px">{api::n(&c, "doneToday")}</span>
                            </div>
                        }
                    }).collect_view()}
                </div>
                <div class="small cap" style="padding:14px 24px;max-width:640px">"A consumer is any connection receiving from this entity. Names come from the client id (RabbitMQ: ClientProvidedName; Service Bus: the SDK); set it in the app to tell workers apart."</div>
            </div>
            <div class="side">
                {move || {
                    let v = picked.get();
                    let c = v["consumer"].clone();
                    if c.is_null() { return view! { <div class="small cap">"Pick a consumer to see its connection and the locks it holds."</div> }.into_any(); }
                    let receiving = c["state"] == "receiving";
                    let (i1, i2) = (s(&c, "id"), s(&c, "id"));
                    let sb = s(&c, "protocol") == "amqp1";
                    view! {
                        <div class="mono" style="font:500 17px var(--mono)">{s(&c, "client")}</div>
                        <div class=if receiving { "small ok" } else { "small cap" }>{if receiving { format!("receiving since {}  ·  from {}", api::hm(api::n(&c, "since")), s(&c, "remote")) } else { format!("disconnected {}  ·  {}", c["until"].as_i64().map(api::hm).unwrap_or_default(), s(&c, "reason")) }}</div>
                        {receiving.then(|| view! { <div class="row" style="gap:8px;margin-top:12px">
                            <button class="btn" on:click=move |_| act("disconnect", i1.clone())>"Disconnect it"</button>
                            {sb.then(|| { let i2 = i2.clone(); view! { <button class="btn" on:click=move |_| act("expire-locks", i2.clone())>"Expire its locks"</button> } })}</div> })}
                        <div class="caps" style="margin-top:22px">"Connection"</div>
                        <table class="kv2" style="margin-top:8px">
                            <tr><td>"Client id"</td><td class="mono">{s(&c, "client")}</td></tr>
                            <tr><td>"Link"</td><td class="mono">{s(&c, "link")}</td></tr>
                            <tr><td>"Receive mode"</td><td class="mono">{s(&c, "mode")}</td></tr>
                            <tr><td>"Prefetch"</td><td class="mono">{api::n(&c, "prefetch")}</td></tr>
                            <tr><td>"Lock duration"</td><td class="mono">{if sb { format!("{} s (entity)", api::n(&v, "lock")) } else { "none (unacked until the app answers)".into() }}</td></tr>
                        </table>
                        <div class="caps" style="margin-top:20px">"Locks held"</div>
                        {let l = v["locks"].as_array().cloned().unwrap_or_default(); if l.is_empty() { view! { <div class="small cap" style="margin-top:6px">"none"</div> }.into_any() } else {
                            l.into_iter().map(|k| view! { <div class="row" style="margin-top:6px;gap:12px"><span class="mono small">{format!("#{}", api::n(&k, "seq"))}</span><span class="mono small grow">{s(&k, "subject")}</span>
                                <span class="small attn">{k["lockedUntil"].as_i64().map(|u| format!("{} s left", ((u - api::now() as i64) / 1000).max(0))).unwrap_or("unacked".into())}</span></div> }).collect_view().into_any() }}
                        <div class="caps" style="margin-top:20px">"Last messages"</div>
                        <div style="margin-top:6px">{v["last"].as_array().cloned().unwrap_or_default().into_iter().take(8).map(|h| view! {
                            <div class="row" style="gap:14px;align-items:baseline;padding:4px 0"><span class="mono small cap" style="width:64px">{api::hms(api::n(&h, "at"))}</span>
                                <span><div>{format!("{} #{}", s(&h, "what"), api::n(&h, "seq"))}</div><div class=format!("small {}", match s(&h, "tone").as_str() { "ok" => "ok", "attn" => "attn", "danger" => "danger", _ => "cap" })>{s(&h, "detail")}</div></span></div> }).collect_view()}</div>
                        <div class="small cap" style="margin-top:14px">{if sb { "Disconnect tests the app's reconnect logic: the client gets a forced link detach (amqp:link:detach-forced); its locks stay until they expire." } else { "Disconnect tests the app's reconnect logic: the connection is closed with CONNECTION_FORCED and its unacked messages go back to the queue." }}</div>
                    }.into_any()
                }}
            </div>
        </div>
    }
}
