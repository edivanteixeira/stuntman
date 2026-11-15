//! A13 · Product events: what the app tracks (PostHog, Segment, Mixpanel, GA4), checked against
//! a plan; personal data in properties flagged; collector failures provoked.

use leptos::prelude::*;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
fn Area(active: &'static str, children: Children) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let evs = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/events?limit=500").await { evs.set(v["events"].as_array().cloned().unwrap_or_default()) } });
    });
    let count = move |src: &str| evs.get().iter().filter(|e| s(e, "source") == src).count();
    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    let base = format!("{origin}{}", api::url(""));
    let rows = [("posthog", "PostHog", "capture · batch · flags"), ("segment", "Segment", "track · identify · batch"), ("mixpanel", "Mixpanel", "track · import · engage"), ("ga4", "GA4", "mp/collect · debug")];
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Events"</div><div class="small cap">"what the app tracks"</div></div>
                {rows.into_iter().map(|(k, t, sub)| view! { <div class="mrow"><div class="row"><span>{t}</span><span class="grow"></span><span class="mono small cap">{move || count(k).to_string()}</span></div><div class="small cap">{sub}</div></div> }).collect_view()}
                <div class="small cap" style="padding:16px 20px;white-space:pre-line">{format!("PostHog host: {base}/posthog\nSegment: {base}/segment\nMixpanel: {base}/mixpanel\nGA4: {base}/ga4/mp/collect")}</div>
            </div>
            <div class="page">
                <AreaHead kind="Events  ·  PostHog, Segment, Mixpanel, GA4  ·  the app's analytics".to_string() title="Product events".to_string()
                    tabs=vec![("events", "Events", "/events".into()), ("plan", "Plan", "/events/plan".into()), ("people", "People", "/events/people".into())]
                    counts=Signal::derive(move || vec![("events", evs.get().len().to_string())]) active=active/>
                {children()}
            </div>
        </div>
    }
}

#[component]
pub fn Events() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let list = RwSignal::new(Vec::<Value>::new());
    let kind = RwSignal::new("429".to_string());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/events?limit=200").await { list.set(v["events"].as_array().cloned().unwrap_or_default()) } });
    });
    let provoke = move |_| { let k = kind.get_untracked(); leptos::task::spawn_local(async move { match api::send("POST", "/events/force", json!({"kind": k, "count": 1})).await { Ok(_) => ui::toast("The collector's next answer fails on purpose"), Err(e) => ui::toast_err(e) } }) };
    view! {
        <Area active="events">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px">
                <table class="tbl"><tr><th>"When"</th><th>"Source"</th><th>"Event"</th><th>"User"</th><th>"Properties"</th></tr>
                    {move || list.get().into_iter().map(|e| { let pii = e["pii"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default(); view! {
                        <tr><td class="mono">{api::hms(n(&e, "at"))}</td><td>{s(&e, "source")}</td><td class="mono">{s(&e, "name")}</td><td class="mono">{s(&e, "user")}</td>
                            <td class="mono small">{e["props"].to_string().chars().take(140).collect::<String>()}{(!pii.is_empty()).then(|| view! { <div class="att small">{format!("⚠ personal data: {pii}")}</div> })}</td></tr> } }).collect_view()}</table>
                {move || list.get().is_empty().then(|| view! { <div class="callout" style="margin-top:16px"><div class="t">"No events yet"</div><div class="t2">"Point the analytics SDK's host at Stuntman (left column). Events show here per user."</div></div> })}
                <div class="caps" style="margin-top:20px">"Provoke on the collector"</div>
                <div class="row" style="gap:8px"><select class="select" on:change=move |e| kind.set(event_target_value(&e))>
                    {["400", "413", "429", "slow", "drop_half"].into_iter().map(|k| view! { <option value=k>{k}</option> }).collect_view()}</select>
                    <button class="btn small" on:click=provoke>"Provoke the next batch"</button></div>
            </div>
        </Area>
    }
}

#[component]
pub fn Plan() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let plans = RwSignal::new(Vec::<Value>::new());
    let pick = RwSignal::new(String::new());
    let result = RwSignal::new(Value::Null);
    let (name, events) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/events/plans").await { plans.set(v["plans"].as_array().cloned().unwrap_or_default()) } });
    });
    Effect::new(move |_| {
        live.tick.track();
        let p = if pick.get().is_empty() { plans.get().first().map(|x| s(x, "name")).unwrap_or_default() } else { pick.get() };
        if p.is_empty() { result.set(Value::Null); return }
        leptos::task::spawn_local(async move { if let Ok(v) = api::send("POST", "/events/check", json!({"plan": p})).await { result.set(v) } });
    });
    let save = move |_| {
        let evs: Vec<Value> = events.get_untracked().lines().filter(|l| !l.trim().is_empty()).map(|l| { let (e, p) = l.split_once(':').unwrap_or((l, "")); json!({"event": e.trim(), "props": p.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect::<Vec<_>>()}) }).collect();
        let body = json!({"name": name.get_untracked(), "events": evs});
        leptos::task::spawn_local(async move { match api::send("PUT", "/events/plans", body).await { Ok(_) => ui::toast("Plan saved"), Err(e) => ui::toast_err(e) } });
    };
    view! {
        <Area active="plan">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                <div class="row" style="gap:8px">{move || plans.get().into_iter().map(|p| { let nm = s(&p, "name"); let n2 = nm.clone(); view! {
                    <button class="btn small" class:primary=move || pick.get() == nm on:click=move |_| pick.set(n2.clone())>{s(&p, "name")}</button> } }).collect_view()}</div>
                {move || { let r = result.get(); (!r.is_null()).then(|| view! {
                    <div class="caps">{format!("The plan · {} of {} arrived", r["events"].as_array().map(|a| a.iter().filter(|x| s(x, "state") == "ok").count()).unwrap_or(0), r["events"].as_array().map(|a| a.len()).unwrap_or(0))}</div>
                    <table class="tbl"><tr><th></th><th>"Event"</th><th>"Missing properties"</th><th>"Arrived"</th></tr>
                        {r["events"].as_array().cloned().unwrap_or_default().into_iter().map(|e| { let st = s(&e, "state"); view! { <tr>
                            <td class={match st.as_str() { "ok" => "ok", "missing" => "danger", _ => "att" }}>{match st.as_str() { "ok" => "✓", "missing" => "✕", _ => "!" }}</td>
                            <td class="mono">{s(&e, "event")}</td><td class="mono">{e["missingProps"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default()}</td><td class="mono">{n(&e, "arrived").to_string()}</td></tr> } }).collect_view()}</table> }) }}
                <div class="caps" style="margin-top:16px">"Add a plan · one event a line, properties after a colon"</div>
                <div class="stack" style="max-width:600px"><input class="input" placeholder="name (signup)" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
                    <textarea class="input mono" rows="5" placeholder="signup_completed: plan, workspace_id\nfirst_case_created" prop:value=move || events.get() on:input=move |e| events.set(event_target_value(&e))></textarea>
                    <div><button class="btn primary small" on:click=save>"Save the plan"</button></div></div>
            </div>
        </Area>
    }
}

#[component]
pub fn People() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let list = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/events/people").await { list.set(v["people"].as_array().cloned().unwrap_or_default()) } });
    });
    view! {
        <Area active="people">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px">
                <table class="tbl"><tr><th>"User"</th><th>"Events"</th><th>"Sources"</th><th>"Last"</th></tr>
                    {move || list.get().into_iter().map(|p| view! { <tr><td class="mono"><a class="link" href=format!("/events?user={}", enc(&s(&p, "user")))>{s(&p, "user")}</a></td><td class="mono">{n(&p, "events").to_string()}</td><td>{s(&p, "sources")}</td><td class="mono">{api::ago(n(&p, "last"))}</td></tr> }).collect_view()}</table>
            </div>
        </Area>
    }
}
