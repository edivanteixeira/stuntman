//! B1 · Timeline: everything that reached this environment's services, in each service's words,
//! who did it, and when Stuntman was offline. Rows of one trace group as a journey.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::Value;

use crate::{
    api::{self, enc, s},
    ui,
};

const SERVICES: [(&str, &str, &str); 11] = [("ai", "AI models", "#7A5AF8"), ("files", "Files", "#8C5A2E"), ("email", "E-mail", "#F2A900"), ("whatsapp", "WhatsApp", "#1FA855"), ("sms", "SMS", "#00A3A3"), ("storage", "Storage", "#E5243B"), ("messaging", "Queues", "#E8743B"), ("keyvault", "Key Vault", "#5C5C66"),
    ("identity", "Identity", "#6B4FD8"), ("apis", "HTTP APIs", "#0070C9"), ("webhook", "Webhooks", "#12A0A8")];
const BUCKETS: usize = 60;
const BAND_KEY: &str = "stuntman.timeline.band";

fn service(m: &str) -> (&'static str, &'static str) {
    SERVICES.iter().find(|x| x.0 == m).map(|x| (x.1, x.2)).unwrap_or(match m { "telemetry" => ("Telemetry", "#8A8F98"), _ => ("Environments", "#8A8F98") })
}
fn chip(m: &str) -> impl IntoView {
    let (label, color) = service(m);
    view! { <span class="small cap"><span style=format!("display:inline-block;width:14px;height:3px;border-radius:2px;margin:0 6px 3px 0;vertical-align:middle;background:{color}")></span>{label}</span> }
}
fn who(a: &str) -> String {
    if a == "you" { "you".into() } else if let Some(x) = a.strip_prefix("mcp:") { format!("{x} (MCP)") } else if let Some(x) = a.strip_prefix("api:") { format!("{x} (API)") } else { a.to_string() }
}

#[component]
pub fn Timeline() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let qm = use_query_map();
    let range = RwSignal::new("15m".to_string());
    let svc = RwSignal::new(String::new());
    let actor = RwSignal::new(String::new());
    let show = RwSignal::new("all".to_string());
    let q = RwSignal::new(String::new());
    let group = RwSignal::new(true);
    let paused = RwSignal::new(false);
    let data = RwSignal::new(Value::Null);
    let unanswered = RwSignal::new(Vec::<Value>::new());
    let picked = RwSignal::new(Value::Null);
    // A selected period (and a zoom) live in the URL: ?from=&to= (epoch ms), ?zfrom=&zto=.
    let pair = |a: &str, b: &str| { let m = qm.get_untracked(); m.get(a).and_then(|x| x.parse::<i64>().ok()).zip(m.get(b).and_then(|x| x.parse::<i64>().ok())) };
    let sel = RwSignal::new(pair("from", "to"));
    let zoom = RwSignal::new(pair("zfrom", "zto"));
    let collapsed = RwSignal::new(web_sys::window().and_then(|w| w.local_storage().ok().flatten()).and_then(|st| st.get_item(BAND_KEY).ok().flatten()).as_deref() == Some("collapsed"));
    let nav = use_navigate();
    Effect::new(move |prev: Option<()>| {
        let (sl, zm) = (sel.get(), zoom.get());
        if prev.is_none() { return; }
        let mut q = vec![];
        if let Some((a, b)) = sl { q.push(format!("from={a}&to={b}")); }
        if let Some((a, b)) = zm { q.push(format!("zfrom={a}&zto={b}")); }
        let url = if q.is_empty() { "/timeline".to_string() } else { format!("/timeline?{}", q.join("&")) };
        nav(&url, leptos_router::NavigateOptions { replace: true, scroll: false, ..Default::default() });
    });
    Effect::new(move |_| {
        if !paused.get() { live.tick.track(); }
        let sh = show.get();
        let mut url = format!("/timeline?range={}&service={}&who={}&show={}&q={}&buckets={BUCKETS}", range.get(), enc(&svc.get()), enc(&actor.get()), if sh == "unanswered" { "all" } else { &sh }, enc(&q.get()));
        if let Some((a, b)) = zoom.get() { url += &format!("&from={a}&to={b}"); }
        if let Some((a, b)) = sel.get() { url += &format!("&sel_from={a}&sel_to={b}"); }
        let since = range.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&url).await { data.set(v); }
            // Calls no route took (HTTP APIs answered 404 "no route"): amber rows on top.
            if let Ok(v) = api::get(&format!("/apis/unmatched?since={since}")).await { unanswered.set(v["groups"].as_array().cloned().unwrap_or_default()); }
        });
    });
    let rows = Memo::new(move |_| if show.get() == "unanswered" { vec![] } else { data.get()["rows"].as_array().cloned().unwrap_or_default() });
    let n_unanswered = move || unanswered.get().iter().map(|g| api::n(g, "count")).sum::<i64>();
    // The open row follows ?id=, else the newest.
    Effect::new(move |_| {
        let id = qm.get().get("id").or_else(|| rows.get().first().map(|r| s(r, "id")));
        let Some(id) = id else { picked.set(Value::Null); return };
        if s(&picked.get_untracked()["row"], "id") == id { return; }
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/timeline/{}", enc(&id))).await { picked.set(x); } });
    });
    view! {
        <div class="page">
            <div class="head" style="height:auto;min-height:88px">
                <div class="top">
                    <div class="titled">
                        <div class="kind">{move || format!("Overview  ·  {}", s(&env.get(), "env").to_uppercase())}</div>
                        <h1>"Timeline"<span class="sub">"everything that happened, in each service's words, and who did it"</span></h1>
                    </div>
                    <span class="grow"></span>
                    {move || zoom.get().is_some().then(|| view! { <button class="link" on:click=move |_| zoom.set(None)>{move || format!("Back to {}", match range.get().as_str() { "15m" => "15 min", "1h" => "1 h", _ => "24 h" })}</button> })}
                    <ui::Seg options=vec![("15m", "15 min"), ("1h", "1 h"), ("24h", "24 h")] value=range set=Callback::new(move |v| { zoom.set(None); sel.set(None); range.set(v) })/>
                    <input class="input" style="width:200px" placeholder="Text, id, person…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                    <span class="small" style="display:flex;align-items:center;gap:6px"><span class="dot" class:off=move || paused.get()></span>{move || if paused.get() { "Paused" } else { "Live" }}</span>
                    <button class="btn" on:click=move |_| paused.update(|p| *p = !*p)>{move || if paused.get() { "Resume" } else { "Pause" }}</button>
                </div>
                <Lanes data=data sel=sel zoom=zoom collapsed=collapsed/>
            </div>
            <div class="body">
                <div class="list narrow kvcol">
                    <div class="caps" style="padding:16px 20px 6px">"Services"</div>
                    {SERVICES.into_iter().map(|(id, label, color)| view! {
                        <div class="kvrow" class:on=move || svc.get() == id on:click=move |_| svc.update(|v| *v = if v == id { String::new() } else { id.to_string() })>
                            <div class="row"><span style=format!("width:14px;height:3px;border-radius:2px;margin-right:10px;background:{color}")></span><span class="grow">{label}</span>
                                <span class="mono small cap">{move || api::thousands(api::n(&data.get()["counts"]["modules"], id))}</span></div></div>
                    }).collect_view()}
                    <div class="caps" style="padding:16px 20px 6px">"Who"</div>
                    {[("app", "the app"), ("you", "you"), ("mcp", "agents (MCP)"), ("api", "tests (API)")].into_iter().map(|(k, label)| view! {
                        <div class="kvrow" class:on=move || actor.get() == k on:click=move |_| actor.update(|v| *v = if v == k { String::new() } else { k.to_string() })>
                            <div>{label}</div><div class="small cap ell">{move || actors(&data.get()["counts"]["actors"], k)}</div></div>
                    }).collect_view()}
                    <div class="row" style="padding:16px 20px 0;gap:10px"><ui::Toggle on=group set=Callback::new(move |v| group.set(v))/><span>"Group by journey"</span></div>
                    <div class="small cap" style="padding:4px 20px">"rows of the same trace together"</div>
                </div>
                <div class="pane">
                    <div class="pills">
                        {[("all", "All"), ("provoked", "Provoked"), ("failed", "Errors")].into_iter().map(|(v, l)| view! {
                            <button class="pill" class:on=move || show.get() == v on:click=move |_| show.set(v.into())>{l}</button> }).collect_view()}
                        <button class="pill warn" class:on=move || show.get() == "unanswered" on:click=move |_| show.set("unanswered".into())
                            title="Calls no route took: Stuntman answered 404 “no route”">
                            {move || { let n = n_unanswered(); if n > 0 { format!("Not answered  {n}") } else { "Not answered".into() } }}</button>
                    </div>
                    {move || matches!(show.get().as_str(), "all" | "unanswered").then(|| {
                        let gs = unanswered.get();
                        if gs.is_empty() && show.get() == "unanswered" {
                            return view! { <div class="empty"><h3>"Every call found a route"</h3><div class="small cap">"Calls to HTTP APIs that no route takes show up here, ready to become a route."</div></div> }.into_any();
                        }
                        view! { <div class="ftable" style="padding:0 24px">{gs.into_iter().map(|g| unanswered_view(g)).collect_view()}</div> }.into_any()
                    })}
                    {move || sel.get().map(|(a, b)| view! {
                        <div class="row selbar">
                            <b>{move || format!("{} of {} events", rows.get().len(), api::n(&data.get(), "windowTotal"))}</b>
                            <span class="selchip">{format!("{} – {}  ·  {}", api::hms(a), api::hms(b), dur(b - a))}<button on:click=move |_| sel.set(None) title="Clear selection">"×"</button></span>
                            {move || (zoom.get() != Some((a, b))).then(|| view! { <button class="link" on:click=move |_| { zoom.set(Some((a, b))); }>"Zoom to selection"</button> })}
                            <span class="grow"></span>
                            {move || { let n = after(&data.get(), b); (n > 0).then(|| view! { <button class="link" on:click=move |_| sel.set(None)>{format!("{n} new after the selection")}</button> }) }}
                        </div> })}
                    {move || (rows.get().is_empty() && show.get() != "unanswered").then(|| if let Some((a, b)) = sel.get() {
                        view! { <div class="empty"><h3>{format!("No events between {} and {}", api::hms(a), api::hms(b))}</h3><button class="btn" on:click=move |_| sel.set(None)>"Clear selection"</button></div> }.into_any()
                    } else { view! { <div class="empty"><h3>"Nothing reached the services in this window"</h3><div class="small cap">"Point the app at this environment, or widen the range."</div></div> }.into_any() })}
                    <div class="ftable">{move || items(rows.get(), data.get()["gaps"].as_array().cloned().unwrap_or_default().into_iter()
                        // With a selection, only the offline stretches that touch it.
                        .filter(|g| sel.get().map(|(a, b)| api::n(g, "to") >= a && api::n(g, "from") <= b).unwrap_or(true)).collect(), group.get()).into_iter().map(|it| item_view(it, picked)).collect_view()}</div>
                </div>
                <div class="side">{move || detail(picked.get())}</div>
            </div>
        </div>
    }
}

/// A request no route took, as an amber row: "POST /v1/refunds: no route in the stripe plugin".
fn unanswered_view(g: Value) -> impl IntoView {
    let l = g["latest"].clone();
    let n = api::n(&g, "count");
    let why = if s(&l, "service").is_empty() { format!("no plugin answers {}", s(&l, "host")) } else { format!("no route in the {} plugin", s(&l, "service")) };
    let id = s(&l, "id");
    let nav = use_navigate();
    view! {
        <div class="frow na">
            <span class="mono small cap" style="width:72px;flex:none;padding-top:14px">{api::hms(api::n(&l, "at"))}</span>
            <span class="grow" style="min-width:0"><span class="small attn">{if n > 1 { format!("Not answered  ·  {n}×") } else { "Not answered".into() }}</span>
                <div>{format!("{} {}: {why}", s(&l, "method"), s(&l, "path"))}</div></span>
            <button class="link" style="align-self:center" on:click=move |_| crate::apis::unmatched::make_route(nav.clone(), id.clone())>"Make a route from it ›"</button>
        </div>
    }
}

fn actors(a: &Value, kind: &str) -> String {
    let names: Vec<String> = a.as_object().into_iter().flatten().map(|(k, _)| k.clone()).filter(|k| match kind {
        "you" => k == "you", "mcp" => k.starts_with("mcp:"), "api" => k.starts_with("api:"),
        _ => !k.is_empty() && k != "you" && !k.starts_with("mcp:") && !k.starts_with("api:") }).map(|k| who(&k).replace(" (MCP)", "").replace(" (API)", "")).collect();
    if kind == "you" { let n = api::n(a, "you"); return if n == 0 { String::new() } else { n.to_string() }; }
    names.join(", ")
}

enum Item { Row(Value), Journey(String, Vec<Value>), Gap(i64, i64) }

/// Rows newest first, a gap where Stuntman was offline, and (grouped) one block per trace.
fn items(rows: Vec<Value>, gaps: Vec<Value>, group: bool) -> Vec<Item> {
    let mut out = vec![];
    let mut done = std::collections::HashSet::new();
    let mut gaps: Vec<(i64, i64)> = gaps.iter().map(|g| (api::n(g, "from"), api::n(g, "to"))).collect();
    gaps.sort_by_key(|g| -g.1);
    let mut gi = 0;
    for r in &rows {
        let at = api::n(r, "at");
        while gi < gaps.len() && gaps[gi].1 > at { out.push(Item::Gap(gaps[gi].0, gaps[gi].1)); gi += 1; }
        let t = s(r, "traceId");
        if group && !t.is_empty() {
            if !done.insert(t.clone()) { continue; }
            let mut j: Vec<Value> = rows.iter().filter(|x| s(x, "traceId") == t).cloned().collect();
            if j.len() > 1 { j.reverse(); out.push(Item::Journey(t, j)); continue; }
        }
        out.push(Item::Row(r.clone()));
    }
    out.extend(gaps[gi..].iter().map(|g| Item::Gap(g.0, g.1)));
    out
}

fn row_view(r: Value, picked: RwSignal<Value>, inner: bool) -> impl IntoView {
    let id = s(&r, "id");
    let (i2, nav) = (id.clone(), use_navigate());
    let outcome = s(&r, "outcome");
    let provoked = outcome == "provoked" || !s(&r, "rule").is_empty();
    let rule = s(&r, "rule");
    let actor = s(&r, "actor");
    let detail = s(&r, "detail").split("  ·  ").filter(|p| *p != actor).collect::<Vec<_>>().join("  ·  ");
    let sub = [if rule.is_empty() { String::new() } else { format!("rule \"{rule}\"") }, who(&actor), detail].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("  ·  ");
    view! {
        <div class="frow" class:on=move || s(&picked.get()["row"], "id") == i2 style=if inner { "padding-left:16px;border-left:3px solid var(--rule)" } else { "" }
            on:click=move |_| nav(&format!("/timeline?id={}", enc(&id)), Default::default())>
            <span class="mono small cap" style="width:72px;flex:none;padding-top:14px">{api::hms(api::n(&r, "at"))}</span>
            <span class="grow" style="min-width:0">{chip(&s(&r, "module"))}
                <div class=match (provoked, outcome.as_str()) { (true, _) => "attn", (_, "failed") => "danger", _ => "" }>{provoked.then_some("◆ ")}{s(&r, "label")}</div>
                <div class="small cap ell">{sub}</div></span>
        </div>
    }
}

fn item_view(it: Item, picked: RwSignal<Value>) -> AnyView {
    match it {
        Item::Row(r) => row_view(r, picked, false).into_any(),
        Item::Gap(a, b) => view! {
            <div class="frow" style="background:var(--danger-fill,#fbe9ea);cursor:default;display:block;box-shadow:inset 3px 0 0 var(--danger)">
                <div class="danger">{format!("Stuntman was offline {}  ·  {} – {}", api::span(b - a), api::hms(a), api::hms(b))}</div>
                <div class="small cap">"restarted; what the app sent in between was not received"</div></div> }.into_any(),
        Item::Journey(t, rows) => {
            let first = rows.first().map(|r| api::n(r, "at")).unwrap_or(0);
            let last = rows.last().map(|r| api::n(r, "at")).unwrap_or(0);
            let head = format!("Journey  ·  {}  ·  trace {}…  ·  {} steps  ·  {}", s(&rows[0], "actor"), &t[..t.len().min(7)], rows.len(), api::span(last - first));
            view! { <div style="border-bottom:1px solid var(--rule)"><div style="padding:10px 24px 2px;font-weight:600;font-size:13px">{head}</div>
                <div style="padding-left:16px">{rows.into_iter().map(|r| row_view(r, picked, true)).collect_view()}</div></div> }.into_any()
        }
    }
}

fn detail(x: Value) -> AnyView {
    let r = x["row"].clone();
    if r.is_null() { return view! { <div class="small cap">"Pick a row to see its journey."</div> }.into_any(); }
    let journey = x["journey"].as_array().cloned().unwrap_or_default();
    let (module, link, trace, rule) = (s(&r, "module"), s(&r, "link"), s(&r, "traceId"), s(&r, "rule"));
    let provoked = s(&r, "outcome") == "provoked" || !rule.is_empty();
    let first = journey.first().map(|j| api::n(j, "at")).unwrap_or(0);
    let total = (journey.last().map(|j| api::n(j, "at")).unwrap_or(0) - first).max(1);
    view! {
        <div class="row" style="gap:8px">{chip(&module)}<span class="small cap">{s(&r, "host")}</span></div>
        <div style="font-weight:600;font-size:17px;margin-top:6px">{s(&r, "label")}</div>
        <div class="small cap">{format!("{}  ·  {}  ·  {}", who(&s(&r, "actor")), s(&r, "detail"), api::hms(api::n(&r, "at")))}</div>
        {provoked.then(|| view! { <div class="callout attn-fill" style="max-width:none;margin:14px 0 0"><div class="t2" style="margin:0">
            {if rule.is_empty() { "Provoked: this answer came from a scenario or a provoke switch, not from a real failure.".to_string() } else { format!("Provoked: rule \"{rule}\".") }}</div></div> })}
        {(journey.len() > 1).then(|| view! {
            <div class="caps" style="margin-top:22px">{format!("Same journey  ·  trace {}…", &trace[..trace.len().min(7)])}</div>
            {journey.iter().map(|j| { let left = (api::n(j, "at") - first) as f64 * 92.0 / total as f64; let (_, color) = service(&s(j, "module"));
                view! { <div class="row" style="gap:10px;padding:4px 0"><span class="small ell" style="width:150px;flex:none">{s(j, "label")}</span>
                    <span class="grow" style="position:relative;height:8px;background:var(--subtle);border-radius:2px">
                        <span style=format!("position:absolute;left:{left:.1}%;width:8%;height:8px;border-radius:2px;background:{color}")></span></span></div> } }).collect_view()}
            <div class="row small cap mono"><span style="width:160px"></span><span class="grow">"0 ms"</span><span>{api::span(total)}</span></div>
        })}
        <div class="caps" style="margin-top:22px">"Open"</div>
        <table class="kv" style="margin-top:6px">
            {(!link.is_empty()).then(|| view! { <tr><td><a class="link" href=link.clone()>{format!("In {} ›", service(&module).0)}</a></td><td class="small cap">"the object or call, and this attempt"</td></tr> })}
            {(!trace.is_empty()).then(|| view! { <tr><td><a class="link" href=format!("/telemetry/traces/{trace}")>"The trace ›"</a></td><td class="small cap">"the app's spans for this request"</td></tr> })}
            <tr><td><button class="link" on:click={ let r = r.clone(); move |_| ui::copy(&serde_json::to_string_pretty(&r).unwrap_or_default(), "row") }>"Copy raw"</button></td><td class="small cap">"the row as JSON (get_timeline_row)"</td></tr>
        </table>
        <div class="small cap" style="margin-top:20px">"Telemetry is not mixed in: spans, logs and metrics open from a row."</div>
    }.into_any()
}

fn dur(ms: i64) -> String {
    let sec = ms / 1000;
    if sec < 60 { format!("{sec} s") } else if sec % 60 == 0 { format!("{} min", sec / 60) } else { format!("{} min {} s", sec / 60, sec % 60) }
}

/// Events in the window after a time (from the lanes' columns): "N new after the selection".
fn after(d: &Value, t: i64) -> i64 {
    let (from, size) = (api::n(d, "from"), api::n(&d["lanes"], "bucketMs").max(1));
    let first = ((t - from) / size + 1).max(0) as usize;
    d["lanes"]["modules"].as_object().into_iter().flatten().map(|(_, v)| v.as_array().map(|a| a.iter().skip(first).filter_map(|x| x.as_i64()).sum::<i64>()).unwrap_or(0)).sum()
}

/// B1 v3 · the overview band: one lane per service, density per column, failures as red dots,
/// a time axis; drag to select a period (the list follows), click a column, double-click clears.
#[component]
fn Lanes(data: RwSignal<Value>, sel: RwSignal<Option<(i64, i64)>>, zoom: RwSignal<Option<(i64, i64)>>, collapsed: RwSignal<bool>) -> impl IntoView {
    let area = NodeRef::<leptos::html::Div>::new();
    let drag = RwSignal::new(None::<(f64, f64)>);
    let window_ms = move || { let d = data.get_untracked(); (api::n(&d, "from"), api::n(&d, "to")) };
    let frac = move |x: f64| -> f64 { area.get_untracked().map(|el| { let r = el.get_bounding_client_rect(); ((x - r.left()) / r.width().max(1.0)).clamp(0.0, 1.0) }).unwrap_or(0.0) };
    let to_ms = move |f: f64| { let (a, b) = window_ms(); a + ((b - a) as f64 * f) as i64 };
    let set_collapsed = move |c: bool| {
        collapsed.set(c);
        if let Some(st) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) { let _ = st.set_item(BAND_KEY, if c { "collapsed" } else { "open" }); }
    };
    let finish = move || {
        if let Some((a, b)) = drag.get_untracked() {
            let (lo, hi) = (a.min(b), a.max(b));
            if hi - lo < 0.004 {
                // A click: the column under the pointer.
                let d = data.get_untracked();
                let (from, size) = (api::n(&d, "from"), api::n(&d["lanes"], "bucketMs").max(1));
                let c = (to_ms(lo) - from) / size;
                sel.set(Some((from + c * size, from + (c + 1) * size)));
            } else {
                sel.set(Some((to_ms(lo), to_ms(hi))));
            }
        }
        drag.set(None);
    };
    let on_key = window_event_listener(leptos::ev::keydown, move |e| if e.key() == "Escape" { drag.set(None); });
    on_cleanup(move || on_key.remove());
    view! {
        <div class="band">
            <div class="row band-top">
                <span class="caps">"Activity"</span><span class="grow"></span>
                <span class="small cap">{move || if collapsed.get() { "" } else { "Drag to select a period  ·  double-click clears" }}</span>
                <button class="btn icon small" title=move || if collapsed.get() { "Show the lanes" } else { "Hide the lanes" } on:click=move |_| set_collapsed(!collapsed.get_untracked())>{move || if collapsed.get() { "▾" } else { "▴" }}</button>
            </div>
            <div class="row band-body">
                <div class="band-labels">{move || (!collapsed.get()).then(|| SERVICES.iter().map(|(_, l, c)| view! { <div class="lab"><span class="ldot" style=format!("background:{c}")></span>{*l}</div> }).collect_view())}</div>
                <div class="band-area" node_ref=area
                    on:mousedown=move |e| { if e.button() == 0 { e.prevent_default(); let f = frac(e.client_x() as f64); drag.set(Some((f, f))); } }
                    on:mousemove=move |e| { if let Some((a, _)) = drag.get_untracked() { drag.set(Some((a, frac(e.client_x() as f64)))); } }
                    on:mouseup=move |_| finish()
                    on:mouseleave=move |_| if drag.get_untracked().is_some() { finish() }
                    on:dblclick=move |_| { sel.set(None); drag.set(None); }>
                    {move || {
                        let d = data.get();
                        let (from, to) = (api::n(&d, "from"), api::n(&d, "to"));
                        let span = (to - from).max(1) as f64;
                        let size = api::n(&d["lanes"], "bucketMs").max(1);
                        let nb = BUCKETS as f64;
                        let lanes = &d["lanes"]["modules"];
                        let pct = |t: i64| ((t - from) as f64 / span * 100.0).clamp(0.0, 100.0);
                        let col = |n: Option<i64>| n.unwrap_or(0);
                        let body = if collapsed.get() {
                            // One line: the total per column.
                            let tot: Vec<i64> = (0..BUCKETS).map(|b| SERVICES.iter().map(|(id, _, _)| col(lanes[*id][b].as_i64())).sum()).collect();
                            let mx = tot.iter().copied().max().unwrap_or(1).max(1) as f64;
                            view! { <div class="lane" style="height:28px">{tot.into_iter().enumerate().filter(|(_, n)| *n > 0).map(|(b, n)| view! {
                                <div class="bar" style=format!("left:{:.3}%;width:{:.3}%;height:{:.0}px;background:var(--text-2)", b as f64 / nb * 100.0, 100.0 / nb * 0.8, 3.0 + 22.0 * n as f64 / mx)></div> }).collect_view()}</div> }.into_any()
                        } else {
                            SERVICES.iter().map(|(id, _, c)| {
                                let vals: Vec<i64> = (0..BUCKETS).map(|b| col(lanes[*id][b].as_i64())).collect();
                                let mx = vals.iter().copied().max().unwrap_or(1).max(1) as f64;
                                let fails: Vec<i64> = d["lanes"]["failed"].as_array().cloned().unwrap_or_default().into_iter().filter(|f| s(f, "module") == *id).map(|f| api::n(&f, "bucket")).collect();
                                view! { <div class="lane">
                                    {vals.into_iter().enumerate().filter(|(_, n)| *n > 0).map(|(b, n)| view! {
                                        <div class="bar" style=format!("left:{:.3}%;width:{:.3}%;height:{:.0}px;background:{c}", b as f64 / nb * 100.0, 100.0 / nb * 0.8, 3.0 + 14.0 * n as f64 / mx)></div> }).collect_view()}
                                    {fails.into_iter().map(|b| view! { <div class="fdot" style=format!("left:calc({:.3}% - 3px)", (b as f64 + 0.4) / nb * 100.0)></div> }).collect_view()}
                                </div> }
                            }).collect_view().into_any()
                        };
                        // Hover: what each column holds.
                        let cols = (0..BUCKETS).map(|b| {
                            let parts: Vec<String> = SERVICES.iter().filter_map(|(id, l, _)| { let n = col(lanes[*id][b].as_i64()); (n > 0).then(|| format!("{l} {n}")) }).collect();
                            let a = from + b as i64 * size;
                            let tip = format!("{} – {}{}", api::hms(a), api::hms(a + size), if parts.is_empty() { "  ·  nothing".to_string() } else { format!("  ·  {}", parts.join("  ·  ")) });
                            view! { <div class="hcol" title=tip style=format!("left:{:.3}%;width:{:.3}%", b as f64 / nb * 100.0, 100.0 / nb)></div> }
                        }).collect_view();
                        let gaps = d["gaps"].as_array().cloned().unwrap_or_default().into_iter().map(|g| { let (a, b) = (pct(api::n(&g, "from")), pct(api::n(&g, "to")));
                            view! { <div class="gapband" style=format!("left:{a:.2}%;width:{:.2}%", (b - a).max(0.3)) title="Stuntman was off"></div> } }).collect_view();
                        // The selection (or the drag in progress): normal colours inside, greyed outside.
                        let shown = drag.get().map(|(a, b)| ((a.min(b) * 100.0), (a.max(b) * 100.0), true))
                            .or_else(|| sel.get().map(|(a, b)| (pct(a), pct(b), false)));
                        let selv = shown.map(|(l, r, dragging)| {
                            let (ms0, ms1) = if dragging { (to_ms(l / 100.0), to_ms(r / 100.0)) } else { sel.get().unwrap_or((0, 0)) };
                            let n: i64 = if dragging { 0 } else { api::n(&d["counts"], "total").max(d["rows"].as_array().map(|a| a.len() as i64).unwrap_or(0)) };
                            view! {
                                <div class="dim" style=format!("left:0;width:{l:.2}%")></div>
                                <div class="dim" style=format!("left:{r:.2}%;right:0")></div>
                                <div class="selbox" style=format!("left:{l:.2}%;width:{:.2}%", (r - l).max(0.2))><span class="h l"></span><span class="h r"></span></div>
                                <div class="sellabel" style=format!("left:{l:.2}%")>{format!("{} – {}  ·  {}{}", api::hms(ms0), api::hms(ms1), dur(ms1 - ms0), if dragging { String::new() } else { format!("  ·  {n} events") })}</div>
                            }
                        });
                        view! { {gaps}{body}{cols}{selv} }
                    }}
                </div>
            </div>
            <div class="row band-axis">
                <div class="band-labels"></div>
                <div class="axis">{move || {
                    let d = data.get();
                    let (from, to) = (api::n(&d, "from"), api::n(&d, "to"));
                    let span = (to - from).max(1);
                    let step: i64 = [60_000, 120_000, 300_000, 600_000, 1_800_000, 3_600_000, 10_800_000].into_iter().find(|s| span / s <= 16).unwrap_or(21_600_000);
                    let first = (from / step + 1) * step;
                    (0..).map(|i| first + i * step).take_while(|t| *t <= to).enumerate().map(|(i, t)| {
                        let p = (t - from) as f64 / span as f64 * 100.0;
                        view! { <div class="tick" style=format!("left:{p:.2}%")>{(i % if span / step > 8 { 3 } else { 1 } == 0).then(|| view! { <span>{api::hm(t)}</span> })}</div> }
                    }).collect_view()
                }}</div>
            </div>
        </div>
    }
}
