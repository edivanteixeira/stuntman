//! A9c · Services › <recorded service> › Review: what the recording found, route by route, with
//! the answers each gave, the template its default answer became and the rule that tells the
//! answers apart. Unchecked routes are left out; "Create the plugin" writes the rest.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui,
};

#[component]
pub fn Review() -> impl IntoView {
    let p = use_params_map();
    let nav = use_navigate();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let data = RwSignal::new(Value::Null);
    let off = RwSignal::new(BTreeSet::<String>::new());
    let rules = RwSignal::new(true);
    let sel = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let load = move || {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/plugins/{}/recording?review=true", enc(&sv))).await {
                if sel.get_untracked().is_empty() {
                    sel.set(v["review"]["routes"][0]["route"].as_str().unwrap_or("").to_string());
                }
                data.set(v);
            }
        });
    };
    Effect::new(move |_| { service.track(); load(); });
    let routes = move || data.get()["review"]["routes"].as_array().cloned().unwrap_or_default();
    let chosen = move || routes().into_iter().filter(|r| r["exists"] != true && !off.get().contains(&s(r, "route"))).collect::<Vec<_>>();
    let nav2 = nav.clone();
    let make = move |_| {
        let sv = service.get_untracked();
        let list: Vec<String> = chosen().iter().map(|r| s(r, "route")).collect();
        let nav = nav2.clone();
        busy.set(true);
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/plugins/{}/recording/plugin", enc(&sv)), json!({"routes": list, "rules": rules.get_untracked()})).await {
                Ok(v) => { ui::toast(format!("Plugin written: {} route(s), {} answer(s). The provider is no longer called.", v["routes"].as_array().map(|a| a.len()).unwrap_or(0), api::n(&v, "answers"))); nav(&format!("/apis/{}/routes", enc(&sv)), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
            busy.set(false);
        });
    };
    let more = move |_| {
        let (sv, d) = (service.get_untracked(), data.get_untracked());
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/plugins/{}/recording", enc(&sv)), json!({"upstream": s(&d, "upstream"), "mode": s(&d, "mode")})).await {
                Ok(_) => nav(&format!("/apis/{}/calls", enc(&sv)), Default::default()),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="review" right=move || ()/> }}
            <div class="body">
                <div class="pane pad" style="flex:1.3">
                    {move || {
                        let d = data.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        if d["recording"] == false && d["state"].is_null() {
                            return view! { <div class="callout"><div class="t">"Nothing recorded"</div><div class="t2">"This service is not recording. Start a recording from + New service, or with start_recording."</div></div> }.into_any();
                        }
                        let n = api::n(&d, "calls");
                        view! {
                            <div style="font-weight:700;font-size:15px">{format!("What Stuntman found in {n} call{}", if n == 1 { "" } else { "s" })}</div>
                            <div class="t2" style="margin:4px 0 18px">{format!("{} masked in what was kept. Uncheck what the plugin should not have; everything stays editable after.", api::n(&d, "masked"))}</div>
                            <table class="tbl"><thead><tr><th></th><th>"Route"</th><th>"Calls"</th><th>"Answers"</th></tr></thead><tbody>
                            {routes().into_iter().map(|r| {
                                let key = s(&r, "route");
                                let (k1, k2, k3) = (key.clone(), key.clone(), key.clone());
                                let exists = r["exists"] == true;
                                view! { <tr class:on=move || sel.get() == k1 on:click=move |_| sel.set(k2.clone()) style="cursor:pointer">
                                    <td><input type="checkbox" disabled=exists prop:checked=move || !exists && !off.get().contains(&k3) on:click=|e| e.stop_propagation() on:change={ let k = key.clone(); move |e| { let on = event_target_checked(&e); off.update(|o| { if on { o.remove(&k); } else { o.insert(k.clone()); } }); } }/></td>
                                    <td class="mono small">{key.clone()}{exists.then(|| view! { <span class="small cap">"  ·  the service has it"</span> })}</td>
                                    <td class="mono small cap">{api::n(&r, "calls")}</td>
                                    <td class="small t2">{r["answers"].as_array().cloned().unwrap_or_default().iter().map(|a| s(a, "name")).collect::<Vec<_>>().join(" · ")}</td></tr> }
                            }).collect_view()}
                            </tbody></table>
                            {d["review"]["grouped"].as_array().filter(|g| !g.is_empty()).map(|g| view! {
                                <div class="caps" style="margin:22px 0 8px">"How paths were grouped"</div>
                                {g.iter().map(|x| view! { <div class="t2 small" style="margin-bottom:4px">{format!("{}{} became {}", x["paths"].as_array().cloned().unwrap_or_default().iter().map(|p| p.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", "), if api::n(x, "more") > 0 { format!(" and {} more", api::n(x, "more")) } else { String::new() }, s(x, "route"))}</div> }).collect_view()}
                                <div class="small cap">"Split them back in the plugin YAML if they are different routes."</div>
                            })}
                            <div class="row" style="margin-top:26px">
                                <button class="btn primary" disabled=move || busy.get() || chosen().is_empty() on:click=make.clone()>{move || { let c = chosen(); format!("Create the plugin · {} route{}", c.len(), if c.len() == 1 { "" } else { "s" }) }}</button>
                                <button class="btn" on:click=more.clone()>"Record more"</button>
                                <label class="check"><input type="checkbox" prop:checked=move || rules.get() on:change=move |e| rules.set(event_target_checked(&e))/>"Keep the suggested rules"</label></div>
                        }.into_any()
                    }}
                </div>
                <div class="pane pad" style="border-left:1px solid var(--rule);background:var(--surface)">
                    {move || {
                        let Some(r) = routes().into_iter().find(|r| s(r, "route") == sel.get()) else { return view! { <div class="cap">"Select a route"</div> }.into_any() };
                        view! {
                            <div class="mono" style="font-weight:500;font-size:13px">{format!("{}  ·  {} calls", s(&r, "route"), api::n(&r, "calls"))}</div>
                            <div class="caps" style="margin:18px 0 8px">"Answers found"</div>
                            {r["answers"].as_array().cloned().unwrap_or_default().into_iter().map(|a| { let st = api::n(&a, "status"); let (good, bad) = (st < 300, st >= 400); view! {
                                <div class="row small" style="margin-bottom:6px"><span class="mono" style="width:150px">{s(&a, "name")}</span>
                                    <span class:ok=good class:danger=bad>{format!("{st}  ·  {} call{}{}", api::n(&a, "calls"), if api::n(&a, "calls") == 1 { "" } else { "s" }, if a["file"] == true { "  ·  kept as a file" } else { "" })}</span></div> } }).collect_view()}
                            {r["failed"].as_array().filter(|f| !f.is_empty()).map(|f| view! { <div class="small attn" style="margin-top:6px">{format!("{} call(s) got no answer from the provider: left out", f.len())}</div> })}
                            <div class="caps" style="margin:18px 0 8px">{format!("The default answer  ·  {}", s(&r, "default"))}</div>
                            <pre class="code small">{s(&r, "example")}</pre>
                            <div class="small cap" style="margin-top:6px">"Ids and dates became templates; values the app sent are echoed, so the answer follows the request."</div>
                            {r["rules"].as_array().filter(|x| !x.is_empty()).map(|rs| view! {
                                <div class="caps" style="margin:18px 0 8px">"Rule suggested"</div>
                                {rs.iter().map(|x| view! { <pre class="code small">{format!("when: {}\nanswer: {}", s(x, "when"), s(x, "answer"))}</pre><div class="small cap" style="margin:4px 0 10px">{s(x, "why")}</div> }).collect_view()}
                            })}
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
