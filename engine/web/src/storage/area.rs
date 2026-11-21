//! The Storage area (C3): the column of accounts (Azure Blob and S3) beside every tab, with
//! "+ New account" on top; picking another account keeps the tab. The header is the same for
//! every tab: kind on top, the account, then Objects · Rules · Links · Events · Settings.

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate, use_query_map};
use serde_json::Value;

use crate::{
    api::{self, enc, s},
    ui::Pop,
};

/// Every storage account, refreshed on live updates.
pub fn accounts() -> RwSignal<Vec<Value>> {
    let calm = api::calm();
    let list = expect_context::<api::Kept>().storage;
    Effect::new(move |_| {
        calm.track();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/storage/accounts").await { list.set(v["accounts"].as_array().cloned().unwrap_or_default()); }
        });
    });
    list
}

/// The account in the URL, else the first one.
pub fn current(list: &[Value], q: Option<String>) -> Value {
    q.and_then(|a| list.iter().find(|x| s(x, "account") == a).cloned()).or_else(|| list.first().cloned()).unwrap_or(Value::Null)
}

fn tab_of(path: &str) -> &'static str {
    if path.starts_with("/files/links") { "links" }
    else if path.starts_with("/files/events") { "events" }
    else if path.ends_with("/rules") { "rules" }
    else if path.starts_with("/services/") { "settings" }
    else { "objects" }
}

fn href(tab: &str, a: &Value) -> String {
    let (n, k) = (enc(&s(a, "account")), s(a, "kind"));
    match tab {
        "links" => format!("/files/links?account={n}"),
        "events" => format!("/files/events?account={n}"),
        "rules" => format!("/services/{k}/rules?account={n}"),
        "settings" => format!("/services/{k}?account={n}"),
        _ => format!("/files?account={n}"),
    }
}

fn kind_label(a: &Value) -> &'static str { if s(a, "kind") == "s3" { "S3" } else { "Azure Blob" } }

#[component]
pub fn Area(children: Children) -> impl IntoView {
    let nav = use_navigate();
    let loc = use_location();
    let qm = use_query_map();
    let list = accounts();
    let filter = expect_context::<api::Kept>().storage_filter;
    let new_menu = RwSignal::new(false);
    let cur = Memo::new(move |_| s(&current(&list.get(), qm.get().get("account")), "account"));
    view! {
        <div class="area">
            <div class="kcol">
                <div class="kh">"Storage"</div>
                <div class="small cap">{move || format!("{} account{}  ·  Azure Blob and S3", list.get().len(), if list.get().len() == 1 { "" } else { "s" })}</div>
                <span class="anchor" style="margin:0 20px"><button class="btn wide" style="width:100%" on:click=move |_| new_menu.set(true)>"+ New account"</button>
                    <Pop open=new_menu>
                        <a class="mi" href="/services/azure-blob?new=1">"Azure Blob account"</a>
                        <a class="mi" href="/services/s3?new=1">"S3 account"</a>
                    </Pop></span>
                <input class="input" placeholder="Filter" prop:value=move || filter.get() on:input=move |e| filter.set(event_target_value(&e))/>
                <div class="krows">
                    {move || {
                        let f = filter.get().to_lowercase();
                        list.get().into_iter().filter(|a| f.is_empty() || s(a, "account").contains(&f)).map(|a| {
                            let n = s(&a, "account");
                            let nav = nav.clone();
                            let a2 = a.clone();
                            view! {
                                <div class="krow" class:on=move || cur.get() == n
                                    on:click=move |_| nav(&href(tab_of(&loc.pathname.get_untracked()), &a2), Default::default())>
                                    <div class="row"><span class="grow">{s(&a, "account")}</span><span class="mono small cap">{api::thousands(api::n(&a, "objects"))}</span></div>
                                    <div class="small cap">{format!("{}  ·  {} {}", kind_label(&a), a["containers"].as_array().map(|x| x.len()).unwrap_or(0), if s(&a, "kind") == "s3" { "bucket(s)" } else { "container(s)" })}</div>
                                </div>
                            }
                        }).collect_view()
                    }}
                </div>
                <div class="small cap kfoot">"Each account is a storage account (Azure) or a set of buckets (S3) your app connects to."</div>
            </div>
            {children()}
        </div>
    }
}

/// The header of every storage tab.
#[component]
pub fn StorageHead(active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let qm = use_query_map();
    let env = expect_context::<RwSignal<Value>>();
    let list = accounts();
    let cur = Memo::new(move |_| current(&list.get(), qm.get().get("account")));
    let tabs: [(&'static str, &'static str); 5] = [("objects", "Objects"), ("rules", "Rules"), ("links", "Links"), ("events", "Events"), ("settings", "Settings")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("Storage  ·  {}  ·  {}", kind_label(&cur.get()), api::s(&env.get(), "env").to_uppercase())}</div>
                    <h1>{move || { let c = cur.get(); if c.is_null() { "Storage".to_string() } else { s(&c, "account") } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label)| {
                    let nav = nav.clone();
                    view! {
                        <div class="tab" class:on=active == key on:click=move |_| nav(&href(key, &cur.get_untracked()), Default::default())>
                            {label}{(key == "objects").then(|| view! { <span class="n">{move || api::thousands(api::n(&cur.get(), "objects"))}</span> })}</div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}
