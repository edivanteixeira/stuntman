//! The HTTP APIs area (C1/C2): the column of this kind's services sits beside every tab, with
//! "+ New service" on top; picking another service keeps the tab you are on. Also the New service
//! page: install a plugin (or another instance of one) or start an empty one that records.

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate, use_params_map};
use serde_json::{json, Value};

use super::{origin_line, services};
use crate::{
    api::{self, enc, s},
    ui,
};

const TABS: [&str; 6] = ["calls", "routes", "answers", "pages", "auth", "settings"];
const LAST: &str = "stuntman.apis.last";

fn store() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The service last opened in this browser (the rail's HTTP APIs goes back to it).
pub fn last_service() -> Option<String> {
    store()?.get_item(LAST).ok()?.filter(|x| !x.is_empty())
}

/// The service /apis opens: the last one used if it still exists, else the first.
pub fn pick(list: &[Value]) -> String {
    last_service().filter(|l| list.iter().any(|x| s(x, "name") == *l)).or_else(|| list.first().map(|x| s(x, "name"))).unwrap_or_default()
}

#[component]
pub fn Area(children: Children) -> impl IntoView {
    let nav = use_navigate();
    let loc = use_location();
    let p = use_params_map();
    let list = services();
    let filter = RwSignal::new(String::new());
    let cur = Memo::new(move |_| p.get().get("service").unwrap_or_else(|| pick(&list.get())));
    Effect::new(move |_| {
        if let (Some(sv), Some(st)) = (p.get().get("service"), store()) { let _ = st.set_item(LAST, &sv); }
    });
    let tab = move || {
        let path = loc.pathname.get();
        TABS.into_iter().find(|t| path.ends_with(&format!("/{t}"))).unwrap_or("calls")
    };
    let creating = move || loc.pathname.get() == "/apis/new";
    let n2 = nav.clone();
    view! {
        <div class="area">
            <div class="kcol">
                <div class="kh">"HTTP APIs"</div>
                <div class="small cap">{move || format!("{} service{}", list.get().len(), if list.get().len() == 1 { "" } else { "s" })}</div>
                <button class="btn wide" class:primary=creating on:click=move |_| n2("/apis/new", Default::default())>"+ New service"</button>
                <input class="input" placeholder="Filter" prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e))/>
                <div class="krows">
                    {move || {
                        let f = filter.get().to_lowercase();
                        list.get().into_iter().filter(|x| f.is_empty() || s(x, "title").to_lowercase().contains(&f) || s(x, "name").contains(&f)).map(|x| {
                            let n = s(&x, "name");
                            let (n1, n2) = (n.clone(), n.clone());
                            let nav = nav.clone();
                            view! {
                                <div class="krow" class:on=move || !creating() && cur.get() == n1
                                    on:click=move |_| nav(&format!("/apis/{}/{}", enc(&n2), tab()), Default::default())>
                                    <div class="row"><span class="grow">{s(&x, "title")}</span><span class="mono small cap">{api::thousands(api::n(&x, "callsToday"))}</span></div>
                                    <div class="small" class:attn=x["billable"] == true class:cap=x["billable"] != true>{origin_line(&x)}</div>
                                </div>
                            }
                        }).collect_view()
                    }}
                </div>
                <div class="small cap kfoot">"BR: a Brazil-only provider. The line under each service says where its plugin came from."</div>
            </div>
            {children()}
        </div>
    }
}

/// /apis/new: install from a plugin, another instance of one, or an empty service that records.
#[component]
pub fn NewService() -> impl IntoView {
    let nav = use_navigate();
    let data = RwSignal::new(Value::Null);
    let q = RwSignal::new(String::new());
    let empty_name = RwSignal::new(String::new());
    let another = RwSignal::new(String::new());
    let another_name = RwSignal::new(String::new());
    leptos::task::spawn_local(async move { data.set(api::get("/plugins").await.unwrap_or(Value::Null)); });
    let install = move |plugin: String, svc: String| {
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", "/plugins", json!({"plugin": plugin, "service": svc})).await {
                Ok(v) => { ui::toast("Service added"); nav(&format!("/apis/{}/calls", enc(&s(&v, "service"))), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let (i1, i2) = (install.clone(), install);
    view! {
        <div class="page">
            <div class="head">
                <div class="top"><div class="titled"><div class="kind">"HTTP API"</div><h1>"New service"</h1></div></div>
                <div class="tabs"><span class="t2" style="align-self:center">"A service is one provider your app calls. Start from a plugin someone already wrote, or empty."</span></div>
            </div>
            <div class="body"><div class="pane pad">
                <div class="row" style="margin-bottom:12px"><span class="caps grow">"From a plugin"</span>
                    <input class="input" style="width:240px" placeholder="Search plugins…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/></div>
                <div class="pgrid">
                    {move || {
                        let f = q.get().to_lowercase();
                        data.get()["plugins"].as_array().cloned().unwrap_or_default().into_iter()
                            .filter(|p| f.is_empty() || format!("{} {} {}", s(p, "title"), s(p, "description"), s(p, "name")).to_lowercase().contains(&f)).map(|p| {
                            let name = s(&p, "name");
                            let (n1, n2, n3, n4) = (name.clone(), name.clone(), name.clone(), name.clone());
                            let (ia, ib) = (i1.clone(), i1.clone());
                            view! {
                                <div class="pcard">
                                    <div class="nm">{s(&p, "title")}</div>
                                    <div class="small cap">{format!("{}{}  ·  {} route{}", s(&p, "origin"), if s(&p, "region").is_empty() { String::new() } else { format!("  ·  {}", s(&p, "region")) }, api::n(&p, "routes"), if api::n(&p, "routes") == 1 { "" } else { "s" })}</div>
                                    <div class="t2 d">{s(&p, "description")}</div>
                                    {if p["installed"] == true {
                                        view! { <div class="row small">
                                            {move || if another.get() == n1 { view! {
                                                <input class="input mono" autofocus style="width:150px;height:28px" placeholder=format!("{}-2", n2) prop:value=move || another_name.get() on:input=move |e| another_name.set(event_target_value(&e).trim().to_lowercase())/>
                                                <button class="btn small primary" disabled=move || another_name.get().is_empty() on:click={ let n = n3.clone(); let ia = ia.clone(); move |_| ia(n.clone(), another_name.get_untracked()) }>"Add"</button>
                                                <button class="link" on:click=move |_| another.set(String::new())>"Cancel"</button>
                                            }.into_any() } else { view! {
                                                <span class="t2">"Installed"</span><span class="cap">"·"</span>
                                                <a class="link" href=format!("/apis/{}/calls", enc(&n4))>"Open"</a><span class="cap">"·"</span>
                                                <button class="link" on:click={ let n = n4.clone(); move |_| { another_name.set(String::new()); another.set(n.clone()) } }>"Add another"</button>
                                            }.into_any() }}
                                        </div> }.into_any()
                                    } else {
                                        let n = name.clone();
                                        view! { <div><button class="btn small" on:click=move |_| ib(n.clone(), String::new())>"Install"</button></div> }.into_any()
                                    }}
                                </div>
                            }
                        }).collect_view()
                    }}
                </div>
                <div class="caps" style="margin:28px 0 12px">"Or start empty"</div>
                <div class="callout" style="max-width:900px"><div class="t2">"Answers 200 to every call and records it. Build its routes from Unmatched or in the route editor."</div>
                    <div class="row" style="margin-top:10px"><span class="small cap">"Name"</span>
                        <input class="input mono" style="width:240px" placeholder="payments-partner" prop:value=move || empty_name.get() on:input=move |e| empty_name.set(event_target_value(&e).trim().to_lowercase())/>
                        <button class="btn primary" disabled=move || empty_name.get().is_empty() on:click=move |_| i2("empty".into(), empty_name.get_untracked())>"Create"</button></div></div>
            </div></div>
        </div>
    }
}
