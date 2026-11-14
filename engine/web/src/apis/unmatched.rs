//! A2 · APIs › Unmatched: requests that reached Stuntman but no route took (answered 404 "no
//! route"), with the closest route and "Create a route from this request"; below, hosts the app
//! called directly, seen in telemetry.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use super::UnmatchedHead;
use crate::{
    api::{self, enc, s},
    ui,
};

#[component]
pub fn Unmatched() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let since = RwSignal::new("1h".to_string());
    let q = RwSignal::new(String::new());
    let group = RwSignal::new("path".to_string());
    let groups = RwSignal::new(Vec::<Value>::new());
    let real = RwSignal::new(Vec::<Value>::new());
    let sel = RwSignal::new(Value::Null);
    let total = RwSignal::new(0i64);
    let load = move || {
        let (g, si) = (group.get_untracked(), since.get_untracked());
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/apis/unmatched?group={g}&since={si}")).await {
                let gs = v["groups"].as_array().cloned().unwrap_or_default();
                total.set(gs.iter().map(|x| api::n(x, "count")).sum());
                if sel.get_untracked().is_null() {
                    sel.set(gs.first().cloned().unwrap_or(Value::Null));
                }
                groups.set(gs);
            }
            if let Ok(v) = api::get(&format!("/apis/real-provider-calls?since={si}")).await {
                real.set(v["hosts"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    Effect::new(move |_| { group.track(); since.track(); sel.set(Value::Null); load(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "apis.unmatched" { load(); }
        t
    });
    let calls = RwSignal::new(0i64);
    leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/apis/services").await {
            calls.set(v["services"].as_array().map(|a| a.iter().map(|x| api::n(x, "callsToday")).sum()).unwrap_or(0));
        }
    });
    let create = {
        let nav = nav.clone();
        move |id: String| {
            let nav = nav.clone();
            leptos::task::spawn_local(async move {
                match api::send("POST", &format!("/apis/unmatched/{}/route", enc(&id)), json!({})).await {
                    Ok(v) if !s(&v, "service").is_empty() => nav(&format!("/apis/{}/routes?draft={}", enc(&s(&v, "service")), enc(&id)), Default::default()),
                    Ok(_) => ui::toast_err("No plugin answers this host: install one from the catalog, or an empty one that records"),
                    Err(e) => ui::toast_err(e),
                }
            });
        }
    };
    let ignore = move |id: String| leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/apis/unmatched/{}/ignore", enc(&id)), json!({})).await { Ok(_) => { sel.set(Value::Null); ui::toast("Ignored; still answered 404") } Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="page">
            <UnmatchedHead since=since q=q unmatched=Signal::derive(move || total.get())/>
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">"Group by"</div>
                    {[("path", "Path"), ("service", "Service"), ("caller", "Caller"), ("host", "Host")].into_iter().map(|(v, l)| view! {
                        <div class="crow" class:on=move || group.get() == v on:click=move |_| group.set(v.into())><span>{l}</span></div> }).collect_view()}
                    <div class="small cap" style="padding:16px 24px">"An unmatched request reached Stuntman but no route of any plugin took it. Stuntman answered 404 “no route” and kept it here."</div>
                </div>
                <div class="pane files">
                    <div class="caps" style="padding:18px 24px 8px">"Reached Stuntman, no route"</div>
                    <div class="ftable">
                        {move || {
                            let f = q.get().to_lowercase();
                            groups.get().into_iter().filter(|g| f.is_empty() || g.to_string().to_lowercase().contains(&f)).map(|g| {
                                let l = g["latest"].clone();
                                let id = s(&l, "id");
                                let g2 = g.clone();
                                let host_label = if s(&l, "service").is_empty() { format!("{} (no plugin)", s(&l, "host")) } else { format!("{} host", s(&l, "service")) };
                                let callers = g["callers"].as_array().map(|c| c.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                                view! { <div class="frow" class:on=move || s(&sel.get()["latest"], "id") == id on:click=move |_| sel.set(g2.clone())>
                                    <span class="mono small cap" style="width:52px">{s(&l, "method")}</span>
                                    <div class="grow"><div class="mono">{if group.get_untracked() == "path" { s(&l, "path") } else { s(&g, "key") }}</div>
                                        <div class="small cap">{format!("{host_label}  ·  {callers}")}</div></div>
                                    <span class="mono small attn">{format!("{}×", api::n(&g, "count"))}</span></div> }
                            }).collect_view()
                        }}
                    </div>
                    {move || groups.get().is_empty().then(|| view! { <div class="callout" style="margin:16px 24px"><div class="t">"Every request found a route"</div>
                        <div class="t2">"Telemetry still shows calls that never reached Stuntman; they appear below when there are any."</div></div> })}
                    {move || (!real.get().is_empty()).then(|| view! {
                        <div class="caps" style="padding:22px 24px 8px">"Seen in telemetry, never through Stuntman"</div>
                        {real.get().into_iter().map(|h| view! {
                            <div class="callout attn-fill" style="margin:0 24px 8px"><div class="row"><span class="mono grow">{s(&h, "host")}</span><span class="mono small">{format!("{} calls  ·  {}", api::n(&h, "calls"), s(&h, "services"))}</span></div>
                                <div class="small attn">"The app still calls the real provider: the base URL is not pointed at Stuntman."</div></div> }).collect_view()}
                        <div class="row" style="padding:4px 24px;gap:20px"><a class="link" href="/apis/new">"Stand in for this host ›"</a><span class="small cap">"Catalog: install a plugin, or an empty one that records"</span></div>
                    })}
                </div>
                <div class="side envside">
                    {move || {
                        let g = sel.get();
                        if g.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick a request to see the closest route."</div> }.into_any(); }
                        let l = g["latest"].clone();
                        let id = s(&l, "id");
                        let (i1, i2) = (id.clone(), id);
                        let c = l["closest"].clone();
                        let r = l["request"].clone();
                        let create = create.clone();
                        view! {
                            <h2 class="h2 mono">{format!("{} {}", s(&l, "method"), s(&l, "path"))}</h2>
                            <div class="small cap">{format!("{} times since {}  ·  from {}  ·  answered 404 “no route”", api::n(&g, "count"), api::hm(g["first"].as_i64().unwrap_or(api::n(&l, "at"))), s(&l, "caller"))}</div>
                            <div class="caps" style="margin:20px 0 8px">"Closest route"</div>
                            {if c.is_null() { view! { <div class="small cap">{if s(&l, "service").is_empty() { "No plugin answers this host. Install one from the catalog, or an empty one that records and answers 200." } else { "No route of this plugin is close." }}</div> }.into_any() }
                             else { view! { <div class="mono">{format!("{} · {}", s(&c, "service"), s(&c, "route"))}</div><div class="small attn" style="margin-top:4px">{s(&c, "text")}</div> }.into_any() }}
                            <div class="caps" style="margin:20px 0 8px">"Request"</div>
                            <pre class="yaml envyaml" style="max-height:240px;overflow:auto">{format!("{} {}\n{}\n\n{}", s(&r, "method"), s(&r, "path"),
                                r["headers"].as_object().cloned().unwrap_or_default().into_iter().filter(|(k, _)| k != "host").map(|(k, v)| { let v = v.as_str().unwrap_or("").to_string(); format!("{k}: {}", if k == "authorization" && v.len() > 18 { format!("{}…", &v[..18]) } else { v }) }).collect::<Vec<_>>().join("\n"), s(&r, "body"))}</pre>
                            <div class="row" style="margin-top:14px;gap:10px">
                                <button class="btn primary" on:click=move |_| create(i1.clone())>"Create a route from this request"</button>
                                <button class="btn" on:click=move |_| ignore(i2.clone())>"Ignore"</button>
                            </div>
                            <div class="small cap" style="margin-top:10px">"Create opens the route editor with the method, path and body filled in; the closest route's steps are copied, so its answers can be reused."</div>
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
