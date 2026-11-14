//! M3 · Namespace settings: access (any key, policies with Rotate, managed identity), queues and
//! topics (add inline, remove, keep what the app created), behaviour; the YAML file beside it.
//! Every change is saved to messaging/<namespace>.yaml at once (the file is the source of truth).

use leptos::prelude::*;
use serde_json::{json, Value};

use super::{label, Sel};
use crate::{
    api::{self, enc, s},
    ui::{self, Toggle},
};

#[component]
pub fn Settings() -> impl IntoView {
    let sel = expect_context::<Sel>();
    let adding = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let new_kind = RwSignal::new("queue".to_string());
    let new_lock = RwSignal::new("30s".to_string());
    let new_max = RwSignal::new("10".to_string());
    let new_sessions = RwSignal::new(false);
    let rotate = RwSignal::new(None::<String>);
    let rotate_open = RwSignal::new(false);
    let custom = RwSignal::new(false);
    let patch = move |p: Value, done: &'static str| {
        let x = sel.namespace_untracked();
        let (k, n) = (s(&x, "kind"), s(&x, "namespace"));
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/services/{k}/accounts/{}", enc(&n)), json!({"patch": p})).await { Ok(_) => ui::toast(done), Err(e) => ui::toast_err(e) }
        });
    };
    let add = move |_| {
        let x = sel.namespace_untracked();
        let (k, n) = (s(&x, "kind"), s(&x, "namespace"));
        let b = json!({"entity": new_name.get_untracked(), "kind": new_kind.get_untracked(), "lock": new_lock.get_untracked(), "max_delivery": new_max.get_untracked().parse::<u64>().unwrap_or(10), "sessions": new_sessions.get_untracked()});
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/services/{k}/accounts/{}/entities", enc(&n)), b).await { Ok(_) => { adding.set(false); new_name.set(String::new()); ui::toast("Added") } Err(e) => ui::toast_err(e) }
        });
    };
    view! {
        <div class="body">
            <div class="pane pad form">
                {move || {
                    let x = sel.namespace();
                    if x.is_null() { return view! { <div class="small cap">"Pick or make a namespace."</div> }.into_any(); }
                    let kind = s(&x, "kind");
                    let sb = kind == "azure-service-bus";
                    let b = x["behaviour"].clone();
                    let conn = s(&x, "connection");
                    let c2 = conn.clone();
                    let any = x["access"]["anyKey"] == true;
                    let mi = x["access"]["managedIdentity"] == true;
                    let tp = s(&b, "throughput");
                    view! {
                        <div class="field"><label>"Namespace"</label>
                            <div class="row"><input class="input mono" style="width:300px" disabled=true prop:value=s(&x, "namespace")/><span class="small cap">"used in the connection string; a new name is a new namespace"</span></div></div>
                        <div class="row" style="margin-top:12px;gap:12px"><span class="small" style="width:80px">"Endpoint"</span><span class="mono small grow ell" title=conn.clone()>{conn.split(';').next().unwrap_or("").to_string()}</span>
                            <button class="link" on:click=move |_| ui::copy(&c2, "Connection string")>"Copy"</button></div>
                        <div class="caps" style="margin:22px 0 10px">"Access"</div>
                        <div class="row" style="gap:10px"><Toggle on=any set=move |v: bool| patch(json!({"access": {"any_key": v}}), if v { "Any key works" } else { "Only the keys below work; others get 401" })/>
                            <span>"Accept any key"</span><span class="small cap">{if sb { "off: only the policies below; others get 401" } else { "off: only the users below; others get ACCESS_REFUSED" }}</span></div>
                        {x["keys"].as_array().cloned().unwrap_or_default().into_iter().map(|k| { let p = s(&k, "name"); let p2 = p.clone(); view! {
                            <div class="row" style="gap:14px;margin-top:10px"><span class="mono small" style="width:190px">{p.clone()}</span><span class="mono small grow">{format!("{}  ·  {}", s(&k, "masked"), k["rights"].as_array().map(|r| r.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default())}</span>
                                <button class="link" on:click=move |_| { rotate.set(Some(p2.clone())); rotate_open.set(true); }>"Rotate"</button></div> } }).collect_view()}
                        {sb.then(|| view! { <div class="row" style="gap:10px;margin-top:14px"><Toggle on=mi set=move |v: bool| patch(json!({"access": {"managed_identity": v}}), "Saved")/><span>"Accept managed identity tokens"</span><span class="small cap">"from Identity"</span></div> })}
                        <div class="caps" style="margin:24px 0 10px">{if sb { "Queues and topics" } else { "Queues and exchanges" }}</div>
                        {x["entities"].as_array().cloned().unwrap_or_default().into_iter().map(|e| {
                            let name = s(&e, "name");
                            let (n1, n2) = (name.clone(), name.clone());
                            let created = s(&e, "createdBy");
                            let what = if s(&e, "kind") == "queue" { format!("queue  ·  {}", if e["sessions"] == true { "sessions".to_string() } else { format!("lock {}", s(&e, "lock")) }) }
                                else { let subs = e["subscriptions"].as_array().map(|a| a.len()).unwrap_or(0); format!("{}{}  ·  {subs} {}", if sb { "topic" } else { "exchange" }, if s(&e, "exchangeType").is_empty() { String::new() } else { format!(" ({})", s(&e, "exchangeType")) }, if sb { "subs" } else { "bindings" }) };
                            let xk = kind.clone();
                            let xn = s(&x, "namespace");
                            let (xk2, xn2) = (xk.clone(), xn.clone());
                            view! { <div class="row" style="gap:14px;padding:7px 0;border-bottom:1px solid var(--rule)">
                                <span class="mono" style="width:260px">{name.clone()}</span><span class="small cap grow">{what}</span>
                                {(!created.is_empty()).then(|| view! { <span class="small attn">{format!("made by {created}  ·  ")}</span>
                                    <button class="link" on:click=move |_| { let (k, n, e) = (xk2.clone(), xn2.clone(), n1.clone()); leptos::task::spawn_local(async move { match api::send("POST", &format!("/services/{k}/accounts/{}/entities", enc(&n)), json!({"entity": e})).await { Ok(_) => ui::toast("Kept in the YAML"), Err(x) => ui::toast_err(x) } }); }>"Keep it in the YAML"</button> })}
                                <button class="link plain" on:click=move |_| { let (k, n, e) = (xk.clone(), xn.clone(), n2.clone()); leptos::task::spawn_local(async move { match api::send("DELETE", &format!("/services/{k}/accounts/{}/entities/{}", enc(&n), enc(&e)), json!({})).await { Ok(_) => ui::toast("Removed with its messages"), Err(x) => ui::toast_err(x) } }); }>"Remove"</button>
                            </div> }
                        }).collect_view()}
                        {move || if adding.get() { view! {
                            <div class="callout" style="max-width:520px">
                                <div class="row" style="gap:10px"><input class="input mono" autofocus style="width:220px" placeholder="invoices-to-render" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e).trim().to_string())/>
                                    <select class="select" on:change=move |e| new_kind.set(event_target_value(&e))><option value="queue">"queue"</option><option value="topic">{if sb { "topic" } else { "exchange" }}</option></select></div>
                                {move || (new_kind.get() == "queue").then(|| view! { <div class="row" style="gap:10px;margin-top:10px"><span class="small">"Lock"</span><input class="input mono" style="width:70px" prop:value=move || new_lock.get() on:input=move |e| new_lock.set(event_target_value(&e))/>
                                    <span class="small">"Max delivery"</span><input class="input mono" style="width:60px" prop:value=move || new_max.get() on:input=move |e| new_max.set(event_target_value(&e))/></div>
                                    <div class="row" style="gap:10px;margin-top:10px"><Toggle on=Signal::derive(move || new_sessions.get()) set=move |v: bool| new_sessions.set(v)/><span class="small">"Sessions"</span></div> })}
                                <div class="row" style="gap:14px;margin-top:12px"><button class="link" disabled=move || new_name.get().is_empty() on:click=add>"Add"</button><button class="link plain" on:click=move |_| adding.set(false)>"Cancel"</button></div>
                            </div> }.into_any() } else { view! { <button class="link" style="margin-top:10px;font-size:13px" on:click=move |_| adding.set(true)>{if sb { "+ Add queue or topic" } else { "+ Add queue or exchange" }}</button> }.into_any() }}
                        <div class="caps" style="margin:24px 0 10px">"Behaviour"</div>
                        <div class="row" style="gap:14px"><span style="width:110px">"Latency"</span>
                            <select class="select" on:change=move |e| patch(json!({"behaviour": {"latency": event_target_value(&e)}}), "Saved")>
                                {[("none", "None"), ("50ms", "50 ms"), ("200ms", "200 ms"), ("1000ms", "1 s")].into_iter().map(|(v, l)| { let on = s(&b, "latency") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                            <span class="small cap">"added to every answer"</span></div>
                        <div class="row" style="gap:14px;margin-top:10px"><span style="width:110px">"Throughput"</span>
                            <select class="select" on:change=move |e| { let v = event_target_value(&e); if v == "custom" { custom.set(true) } else { patch(json!({"behaviour": {"throughput": v}}), if v == "throttle_now" { "Throttled: every send gets ServerBusy" } else { "Saved" }) } }>
                                {[("none", "No limit"), ("100", "100 messages a second"), ("10", "10 messages a second"), ("custom", "Custom…"), ("throttle_now", "Throttle now · every send ServerBusy")].into_iter().map(|(v, l)| { let on = tp == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}
                                {(!["none", "100", "10", "throttle_now"].contains(&tp.as_str())).then(|| view! { <option value=tp.clone() selected=true>{format!("{tp} a second")}</option> })}</select>
                            {move || custom.get().then(|| view! { <input class="input mono" style="width:70px" placeholder="50" on:change=move |e| { patch(json!({"behaviour": {"throughput": event_target_value(&e)}}), "Saved"); custom.set(false); }/> })}
                            <span class="small cap">"over it, sends get ServerBusy"</span></div>
                        <div class="row" style="gap:10px;margin-top:14px"><Toggle on=b["auto_create"] == true set=move |v: bool| patch(json!({"behaviour": {"auto_create": v}}), if v { "Queues the app asks for are created" } else { "Off: a missing entity answers 404" })/><span>"Create queues the app asks for"</span></div>
                        <div class="row" style="gap:10px;margin-top:10px"><span style="width:180px">"Keep completed messages"</span>
                            <input class="input mono" style="width:70px" prop:value=s(&b, "keep_completed") on:change=move |e| patch(json!({"behaviour": {"keep_completed": event_target_value(&e)}}), "Saved")/></div>
                        <div class="row" style="gap:10px;margin-top:10px"><Toggle on=b["seed_on_reset"] == true set=move |v: bool| patch(json!({"behaviour": {"seed_on_reset": v}}), "Saved")/><span>{format!("Load {} on reset", s(&x["settings"], "seed"))}</span></div>
                        <div class="small cap" style="margin-top:20px;border-top:1px solid var(--rule);padding-top:12px">{format!("Fields come from the {} plugin. The same file is the source of truth: {}.", label(&kind), s(&x, "file"))}</div>
                    }.into_any()
                }}
            </div>
            <div class="side yaml" style="width:420px">
                <div class="row"><span class="caps grow">{move || s(&sel.namespace(), "file").to_uppercase()}</span><span class="small ok">"live"</span></div>
                <pre class="yaml" style="margin-top:10px;white-space:pre-wrap">{move || s(&sel.namespace(), "yaml")}</pre>
                <div class="small cap" style="margin-top:8px">"Editing the file updates this form; the form writes the file."</div>
            </div>
            <ui::Confirm open=rotate_open question=Signal::derive(move || format!("Rotate the key of “{}”?", rotate.get().unwrap_or_default())) detail=Signal::derive(|| "Senders still using the old key get 401 Unauthorized.".to_string()) action="Rotate"
                on_yes=move |_| { let x = sel.namespace_untracked(); let (k, n, p) = (s(&x, "kind"), s(&x, "namespace"), rotate.get_untracked().unwrap_or_default());
                    leptos::task::spawn_local(async move { match api::send("POST", &format!("/services/{k}/accounts/{}/policies/{}/rotate", enc(&n), enc(&p)), json!({})).await { Ok(_) => ui::toast("New key made: copy the connection string again"), Err(e) => ui::toast_err(e) } }); }/>
        </div>
    }
}
