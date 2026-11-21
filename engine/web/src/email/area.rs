//! The E-mail area: the column of e-mail services beside every tab, with "+ New service" on top;
//! picking another service keeps the tab. Tabs: Messages · Inboxes · Extractors · Settings.

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_navigate, use_params_map, use_query_map};
use serde_json::Value;

use crate::api::{self, enc, s};

pub fn services() -> RwSignal<Vec<Value>> {
    let calm = api::calm();
    let list = expect_context::<api::Kept>().email;
    Effect::new(move |_| {
        calm.track();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/services").await {
                list.set(v["services"].as_array().cloned().unwrap_or_default().into_iter().map(|x| x["service"].clone()).collect());
            }
        });
    });
    list
}

/// The service in the URL (/services/email/s/<name> or ?service=), else the default one.
fn current(list: &[Value], p: Option<String>, q: Option<String>) -> Value {
    p.or(q).and_then(|n| list.iter().find(|x| s(x, "name") == n).cloned())
        .or_else(|| list.iter().find(|x| x["default"] == true).cloned()).or_else(|| list.first().cloned()).unwrap_or(Value::Null)
}

fn tab_of(path: &str) -> &'static str {
    if path.starts_with("/inboxes") { "inboxes" }
    else if path.starts_with("/services/email/extractors") { "extractors" }
    else if path.starts_with("/services/email") { "settings" }
    else { "messages" }
}

fn href(tab: &str, n: &str) -> String {
    let n = enc(n);
    match tab {
        "inboxes" => format!("/inboxes?service={n}"),
        "mailboxes" => "/mailboxes".to_string(),
        "extractors" => format!("/services/email/extractors?service={n}"),
        "settings" => format!("/services/email/s/{n}"),
        _ => format!("/notifications?service={n}"),
    }
}

fn intake(sv: &Value) -> String {
    let mut p = vec![];
    if sv["intake"]["sendgridApi"]["on"] == true { p.push("SendGrid API"); }
    if sv["intake"]["smtp"]["on"] == true { p.push("SMTP"); }
    let p = if p.is_empty() { "no intake on".to_string() } else { p.join(" + ") };
    if sv["default"] == true { format!("default  ·  {p}") } else { p }
}

#[component]
pub fn Area(children: Children) -> impl IntoView {
    let nav = use_navigate();
    let loc = use_location();
    let pm = use_params_map();
    let qm = use_query_map();
    let list = services();
    let cur = Memo::new(move |_| s(&current(&list.get(), pm.get().get("name").filter(|_| loc.pathname.get().starts_with("/services/email/s/")), qm.get().get("service")), "name"));
    let n2 = nav.clone();
    view! {
        <div class="area">
            <div class="kcol">
                <div class="kh">"E-mail"</div>
                <div class="small cap">{move || format!("{} service{}", list.get().len(), if list.get().len() == 1 { "" } else { "s" })}</div>
                <button class="btn wide" on:click=move |_| n2("/services/email?new=1", Default::default())>"+ New service"</button>
                <div class="krows">
                    {move || list.get().into_iter().map(|sv| {
                        let n = s(&sv, "name");
                        let (n1, n3) = (n.clone(), n.clone());
                        let nav = nav.clone();
                        view! {
                            <div class="krow" class:on=move || cur.get() == n1 on:click=move |_| nav(&href(tab_of(&loc.pathname.get_untracked()), &n3), Default::default())>
                                <div>{n.clone()}</div><div class="small cap">{intake(&sv)}</div>
                            </div>
                        }
                    }).collect_view()}
                </div>
                <div class="small cap kfoot">"A service is one way your app sends e-mail (SMTP, the SendGrid API) with its own rules."</div>
            </div>
            {children()}
        </div>
    }
}

/// The header of every e-mail tab.
#[component]
pub fn EmailHead(active: &'static str, #[prop(into)] right: ViewFn, #[prop(optional, into)] tabs_right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let loc = use_location();
    let pm = use_params_map();
    let qm = use_query_map();
    let env = expect_context::<RwSignal<Value>>();
    let list = services();
    let cur = Memo::new(move |_| s(&current(&list.get(), pm.get().get("name").filter(|_| loc.pathname.get().starts_with("/services/email/s/")), qm.get().get("service")), "name"));
    let tabs: [(&'static str, &'static str); 5] = [("messages", "Messages"), ("mailboxes", "Mailboxes"), ("inboxes", "Inboxes"), ("extractors", "Extractors"), ("settings", "Settings")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("E-mail  ·  {}", api::s(&env.get(), "env").to_uppercase())}</div>
                    <h1>{move || { let c = cur.get(); if c.is_empty() { "E-mail".to_string() } else { c } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label)| {
                    let nav = nav.clone();
                    view! { <div class="tab" class:on=active == key on:click=move |_| nav(&href(key, &cur.get_untracked()), Default::default())>{label}</div> }
                }).collect_view()}
                <span class="grow"></span>
                {tabs_right.run()}
            </div>
        </div>
    }
}
