//! The shell every screen shares: the rail, the environment, who is connected, live updates.
//! A new service adds its rail entry here and its routes below.

use leptos::prelude::*;
use leptos_router::{
    components::{Redirect, Route, Router, Routes},
    hooks::{use_location, use_navigate},
    path,
};
use serde_json::Value;

use crate::{ai, api, apis, catalog, email, events, ftp, sms, streams, environments, identity, keyvault, messaging, storage, telemetry, timeline, ui, webhook, whatsapp};

#[component]
pub fn App() -> impl IntoView {
    provide_context(ui::Toasts(RwSignal::new(vec![])));
    let live = api::live();
    provide_context(live);
    let env = RwSignal::new(Value::Null);
    provide_context(env);
    let load = move || {
        leptos::task::spawn_local(async move {
            match api::get("/env").await {
                Ok(v) => env.set(v),
                // The environment picked in this browser was deleted: back to the default one.
                Err(_) if api::stored_env().is_some() => api::switch_env("", "/environments"),
                Err(_) => {}
            }
        });
    };
    load();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && api::s(&live.last.get_untracked(), "topic") == "environments" {
            load();
        }
        t
    });
    view! {
        <Router>
            <div class="app">
                <Rail/>
                <div class="main">
                <environments::Strip/>
                <Routes fallback=|| view! { <div class="empty"><h3>"Nothing here"</h3></div> }>
                    <Route path=path!("/environments") view=environments::list::List/>
                    <Route path=path!("/environments/new") view=environments::new::New/>
                    <Route path=path!("/environments/:name") view=environments::settings::Settings/>
                    <Route path=path!("/") view=|| view! { <Redirect path="/timeline"/> }/>
                    <Route path=path!("/timeline") view=timeline::Timeline/>
                    <Route path=path!("/telemetry") view=|| view! { <Redirect path="/telemetry/traces"/> }/>
                    <Route path=path!("/telemetry/traces") view=telemetry::traces::Traces/>
                    <Route path=path!("/telemetry/traces/:id") view=telemetry::traces::Traces/>
                    <Route path=path!("/telemetry/logs") view=telemetry::logs::Logs/>
                    <Route path=path!("/telemetry/logs/:id") view=telemetry::logs::Logs/>
                    <Route path=path!("/telemetry/metrics") view=telemetry::metrics::Metrics/>
                    <Route path=path!("/telemetry/metrics/:name") view=telemetry::metrics::Metrics/>
                    <Route path=path!("/telemetry/settings") view=telemetry::settings::Settings/>
                    <Route path=path!("/notifications") view=|| view! { <email::area::Area><email::messages::Messages/></email::area::Area> }/>
                    <Route path=path!("/notifications/:id") view=|| view! { <email::area::Area><email::messages::Messages/></email::area::Area> }/>
                    <Route path=path!("/inboxes") view=|| view! { <email::area::Area><email::inboxes::Inboxes/></email::area::Area> }/>
                    <Route path=path!("/inboxes/webhooks") view=webhook::deliveries::Deliveries/>
                    <Route path=path!("/inboxes/webhooks/:name") view=webhook::deliveries::Deliveries/>
                    <Route path=path!("/inboxes/webhooks/:name/behaviour") view=webhook::behaviour::Behaviour/>
                    <Route path=path!("/inboxes/:address") view=|| view! { <email::area::Area><email::inboxes::Inboxes/></email::area::Area> }/>
                    <Route path=path!("/apis") view=|| view! { <apis::area::Area><apis::calls::Calls/></apis::area::Area> }/>
                    <Route path=path!("/apis/unmatched") view=apis::unmatched::Unmatched/>
                    <Route path=path!("/apis/new") view=|| view! { <apis::area::Area><apis::area::NewService/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service") view=|| view! { <apis::area::Area><apis::calls::Calls/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/calls") view=|| view! { <apis::area::Area><apis::calls::Calls/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/routes") view=|| view! { <apis::area::Area><apis::operations::RoutesOrOperations/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/review") view=|| view! { <apis::area::Area><apis::review::Review/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/answers") view=|| view! { <apis::area::Area><apis::answers::Answers/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/pages") view=|| view! { <apis::area::Area><apis::pages::Pages/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/tools") view=|| view! { <apis::area::Area><apis::mcp::Tools/></apis::area::Area> }/>
                    <Route path=path!("/ai") view=ai::Calls/>
                    <Route path=path!("/ai/rules") view=ai::Rules/>
                    <Route path=path!("/ai/spend") view=ai::Spend/>
                    <Route path=path!("/events") view=events::Events/>
                    <Route path=path!("/events/plan") view=events::Plan/>
                    <Route path=path!("/events/people") view=events::People/>
                    <Route path=path!("/ftp") view=ftp::Files/>
                    <Route path=path!("/streams") view=streams::Streams/>
                    <Route path=path!("/mailboxes") view=email::mailboxes::Mailboxes/>
                    <Route path=path!("/identity/protocols") view=identity::protocols::Protocols/>
                    <Route path=path!("/catalog") view=catalog::Store/>
                    <Route path=path!("/catalog/import") view=catalog::Import/>
                    <Route path=path!("/catalog/:plugin") view=catalog::PluginPage/>
                    <Route path=path!("/apis/:service/auth") view=|| view! { <apis::area::Area><apis::auth::Auth/></apis::area::Area> }/>
                    <Route path=path!("/apis/:service/settings") view=|| view! { <apis::area::Area><apis::settings::Settings/></apis::area::Area> }/>
                    <Route path=path!("/services/api") view=|| view! { <Redirect path="/apis/new"/> }/>
                    <Route path=path!("/identity") view=identity::signins::SignIns/>
                    <Route path=path!("/identity/users") view=identity::users::Users/>
                    <Route path=path!("/identity/apps") view=identity::apps::Apps/>
                    <Route path=path!("/identity/tenant") view=identity::tenant::Tenant/>
                    <Route path=path!("/oauth-clients") view=identity::clients::Clients/>
                    <Route path=path!("/secrets") view=keyvault::secrets::Secrets/>
                    <Route path=path!("/secrets/keys") view=keyvault::keys::Keys/>
                    <Route path=path!("/secrets/certificates") view=keyvault::certificates::Certificates/>
                    <Route path=path!("/secrets/log") view=keyvault::log::Log/>
                    <Route path=path!("/secrets/settings") view=keyvault::settings::Settings/>
                    <Route path=path!("/sms") view=sms::Messages/>
                    <Route path=path!("/sms/recipients") view=sms::Recipients/>
                    <Route path=path!("/sms/service") view=sms::Service/>
                    <Route path=path!("/whatsapp") view=whatsapp::conversations::Conversations/>
                    <Route path=path!("/whatsapp/templates") view=whatsapp::templates::Templates/>
                    <Route path=path!("/whatsapp/recipients") view=whatsapp::recipients::Recipients/>
                    <Route path=path!("/whatsapp/number") view=whatsapp::number::Number/>
                    <Route path=path!("/queues") view=|| view! { <messaging::Area><messaging::messages::Messages/></messaging::Area> }/>
                    <Route path=path!("/queues/subscriptions") view=|| view! { <messaging::Area><messaging::subscriptions::Subscriptions/></messaging::Area> }/>
                    <Route path=path!("/queues/dead-letter") view=|| view! { <messaging::Area><messaging::messages::Messages dlq=true/></messaging::Area> }/>
                    <Route path=path!("/queues/consumers") view=|| view! { <messaging::Area><messaging::consumers::Consumers/></messaging::Area> }/>
                    <Route path=path!("/queues/rules") view=|| view! { <messaging::Area><messaging::rules::Rules/></messaging::Area> }/>
                    <Route path=path!("/queues/settings") view=|| view! { <messaging::Area><messaging::settings::Settings/></messaging::Area> }/>
                    <Route path=path!("/files") view=|| view! { <storage::area::Area><storage::objects::Objects/></storage::area::Area> }/>
                    <Route path=path!("/files/links") view=|| view! { <storage::area::Area><storage::links::Links/></storage::area::Area> }/>
                    <Route path=path!("/files/events") view=|| view! { <storage::area::Area><storage::events::Events/></storage::area::Area> }/>
                    <Route path=path!("/services/azure-blob") view=|| view! { <storage::area::Area><storage::accounts::Accounts/></storage::area::Area> }/>
                    <Route path=path!("/services/azure-blob/rules") view=|| view! { <storage::area::Area><storage::rules::Rules/></storage::area::Area> }/>
                    <Route path=path!("/services/s3") view=|| view! { <storage::area::Area><storage::accounts::Accounts/></storage::area::Area> }/>
                    <Route path=path!("/services/s3/rules") view=|| view! { <storage::area::Area><storage::rules::Rules/></storage::area::Area> }/>
                    <Route path=path!("/services") view=|| view! { <Redirect path="/services/email"/> }/>
                    <Route path=path!("/services/email") view=|| view! { <email::area::Area><email::service::Service/></email::area::Area> }/>
                    <Route path=path!("/services/email/s/:name") view=|| view! { <email::area::Area><email::service::Service/></email::area::Area> }/>
                    <Route path=path!("/services/email/extractors") view=|| view! { <email::area::Area><email::extractors::Extractors/></email::area::Area> }/>
                    <Route path=path!("/services/email/extractors/:name") view=|| view! { <email::area::Area><email::extractors::Extractors/></email::area::Area> }/>
                </Routes>
                </div>
            </div>
            <ui::ToastHost/>
        </Router>
    }
}

/// Rail (shell B): global views on top, one entry per service kind (its services are listed in
/// the page's own column, with + New on top), then environments and settings. "" = not in this build.
// Scenarios (B5/B6) is designed but deferred (2026-11-13): it comes back here when built.
const TOP: [(&str, &str); 2] = [("Timeline", "/timeline"), ("Unmatched", "/apis/unmatched")];
const KINDS: [(&str, &str); 14] = [("E-mail", "/notifications"), ("WhatsApp", "/whatsapp"), ("SMS", "/sms"), ("Storage", "/files"), ("Queues", "/queues"), ("Streams", "/streams"), ("Files", "/ftp"), ("Key Vault", "/secrets"), ("Identity", "/identity"),
    ("HTTP APIs", "/apis"), ("AI models", "/ai"), ("Events", "/events"), ("Webhooks", "/inboxes/webhooks"), ("Telemetry", "/telemetry")];
const BOTTOM: [(&str, &str); 3] = [("Environments", "/environments"), ("Catalog", "/catalog"), ("Settings", "")];

/// Which rail entry a path belongs to.
fn rail_on(path: &str) -> &'static str {
    let any = |p: &[&str]| p.iter().any(|x| path == *x || path.starts_with(&format!("{x}/")));
    if path == "/apis/unmatched" { "Unmatched" }
    else if path == "/timeline" { "Timeline" }
    else if any(&["/inboxes/webhooks"]) { "Webhooks" }
    else if any(&["/notifications", "/inboxes", "/services/email", "/mailboxes"]) { "E-mail" }
    else if any(&["/streams"]) { "Streams" }
    else if any(&["/ftp"]) { "Files" }
    else if any(&["/ai"]) { "AI models" }
    else if any(&["/events"]) { "Events" }
    else if any(&["/files", "/services/azure-blob", "/services/s3"]) { "Storage" }
    else if any(&["/apis"]) { "HTTP APIs" }
    else if any(&["/queues"]) { "Queues" }
    else if any(&["/telemetry"]) { "Telemetry" }
    else if any(&["/whatsapp"]) { "WhatsApp" }
    else if any(&["/sms"]) { "SMS" }
    else if any(&["/secrets"]) { "Key Vault" }
    else if any(&["/identity", "/oauth-clients"]) { "Identity" }
    else if any(&["/environments"]) { "Environments" }
    else if any(&["/catalog"]) { "Catalog" }
    else { "" }
}

#[component]
fn Rail() -> impl IntoView {
    let loc = use_location();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let counts = RwSignal::new((0i64, 0i64));
    let clients = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move {
            let un = api::get("/apis/unmatched?since=1h").await.ok().and_then(|v| v["groups"].as_array().map(|g| g.iter().map(|x| api::n(x, "count")).sum::<i64>())).unwrap_or(0);
            let sv = api::get("/apis/services").await.ok().and_then(|v| v["services"].as_array().map(|a| a.len() as i64)).unwrap_or(0);
            counts.set((un, sv));
            if let Ok(c) = api::get("/clients").await {
                clients.set(c["clients"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    view! {
        <nav class="rail">
            <div class="logo">"Stuntman"</div>
            <environments::Switcher/>
            {
                let entry = move |label: &'static str, path: &'static str| {
                    let nav = nav.clone();
                    let count = move || match label {
                        "Unmatched" => { let n = counts.get().0; if n > 0 { n.to_string() } else { String::new() } }
                        "HTTP APIs" => counts.get().1.to_string(),
                        _ => String::new(),
                    };
                    view! {
                        <div class="item" class:on=move || rail_on(&loc.pathname.get()) == label class:off=path.is_empty()
                            title=if path.is_empty() { "Not in this build yet" } else { "" }
                            on:click=move |_| if !path.is_empty() { nav(path, Default::default()) }>
                            <span>{label}</span><span class="count">{count}</span>
                        </div>
                    }
                };
                view! {
                    {TOP.iter().map(|(l, p)| entry(l, p)).collect_view()}
                    <div class="group caps">"Services"</div>
                    {KINDS.iter().map(|(l, p)| entry(l, p)).collect_view()}
                    <div class="rail-bottom">{BOTTOM.iter().map(|(l, p)| entry(l, p)).collect_view()}</div>
                }
            }
            <div class="connected">
                <div class="caps">"Connected"</div>
                {move || {
                    let c = clients.get();
                    if c.is_empty() {
                        view! { <div class="small cap" style="margin-top:10px">"No agent or test yet"</div> }.into_any()
                    } else {
                        // A long list would push into the rail's entries: the first four, then "+N more".
                        let more = c.len().saturating_sub(4);
                        let rest = c.iter().skip(4).map(|x| format!("{} · {}", api::s(x, "name"), api::s(x, "via"))).collect::<Vec<_>>().join("\n");
                        view! {
                            {c.into_iter().take(4).map(|c| view! {
                                <div class="client"><span class="dot"></span><span>{api::s(&c, "name")}</span><span class="via">{api::s(&c, "via")}</span></div>
                            }).collect_view()}
                            {(more > 0).then(|| view! { <div class="small cap" style="margin-top:8px" title=rest>{format!("+{more} more")}</div> })}
                        }.into_any()
                    }
                }}
            </div>
            <div class="foot">{move || format!("Stuntman {}", api::s(&env.get(), "version"))}</div>
        </nav>
    }
}
