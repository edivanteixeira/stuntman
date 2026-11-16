//! The shell every screen shares: the rail, the environment, who is connected, live updates.
//! A new service adds its rail entry here and its routes below.

use leptos::prelude::*;
use leptos_router::{
    components::{Redirect, Route, Router, Routes},
    hooks::{use_location, use_navigate},
    path,
};
use serde_json::Value;

use crate::{access, ai, api, apis, catalog, email, events, ftp, kinesis, pubsub, sms, streams, environments, identity, keyvault, messaging, storage, telemetry, timeline, ui, webhook, whatsapp};

#[component]
pub fn App() -> impl IntoView {
    provide_context(ui::Toasts(RwSignal::new(vec![])));
    let live = api::live();
    provide_context(live);
    let env = RwSignal::new(Value::Null);
    provide_context(env);
    // Who is signed in (sign-in on), where their MCP sessions start.
    let me = Me(RwSignal::new(Value::Null));
    provide_context(me);
    leptos::task::spawn_local(async move { if let Ok(v) = api::get("/whoami").await { me.0.set(v) } });
    api::keep_links_under_base();
    let load = move || {
        leptos::task::spawn_local(async move {
            match api::get("/env").await {
                Ok(v) => { api::set_shared(v["shared"].as_bool() == Some(true)); env.set(v) }
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
        <Router base=api::path_base()>
            <div class="app">
                <Rail/>
                <div class="main">
                <environments::Strip/>
                <Routes fallback=|| view! { <div class="empty"><h3>"Nothing here"</h3></div> }>
                    <Route path=path!("/environments") view=environments::list::List/>
                    <Route path=path!("/environments/new") view=environments::new::New/>
                    <Route path=path!("/environments/:name") view=environments::settings::Settings/>
                    <Route path=path!("/environments/:name/connections") view=environments::connections::Connections/>
                    <Route path=path!("/settings/access") view=access::Access/>
                    <Route path=path!("/") view=move || {
                        // Wait for /api/env: a shared Stuntman's root opens the list of environments.
                        let e = env.get();
                        if e.is_null() { ().into_any() }
                        else if api::shared_root() { view! { <Redirect path="/environments"/> }.into_any() }
                        else { view! { <Redirect path="/timeline"/> }.into_any() }
                    }/>
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
                    <Route path=path!("/pubsub") view=pubsub::PubSub/>
                    <Route path=path!("/kinesis") view=kinesis::Kinesis/>
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
                    <Route path=path!("/queues/activemq") view=messaging::activemq::ActiveMq/>
                    <Route path=path!("/queues/eventhubs") view=messaging::eventhubs::EventHubs/>
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

/// Rail (shell C2): Timeline and Telemetry (how you see the app), then the services, the ones this
/// environment's apps use first, then environments and settings. "" = not in this build.
// Scenarios (B5/B6) is designed but deferred (2026-11-13): it comes back here when built.
// "Not answered" (the old Unmatched) is a badge on Timeline and a filter there; /apis/unmatched still opens.
const TOP: [(&str, &str, &str); 2] = [("Timeline", "/timeline", ""), ("Telemetry", "/telemetry", "telemetry")];
/// (label, path, usage kind)
const KINDS: [(&str, &str, &str); 15] = [("E-mail", "/notifications", "email"), ("WhatsApp", "/whatsapp", "whatsapp"), ("SMS", "/sms", "sms"), ("Storage", "/files", "storage"), ("Queues", "/queues", "queues"),
    ("Streams", "/streams", "streams"), ("Pub/Sub", "/pubsub", "pubsub"), ("Kinesis", "/kinesis", "kinesis"), ("Files", "/ftp", "files"), ("Key Vault", "/secrets", "keyvault"), ("Identity", "/identity", "identity"),
    ("HTTP APIs", "/apis", "apis"), ("AI models", "/ai", "ai"), ("Events", "/events", "events"), ("Webhooks", "/inboxes/webhooks", "webhooks")];
const BOTTOM: [(&str, &str, &str); 3] = [("Environments", "/environments", ""), ("Catalog", "/catalog", ""), ("Settings", "/settings/access", "")];
const SHOW_KEY: &str = "stuntman.rail.services";

/// Which rail entry a path belongs to.
fn rail_on(path: &str) -> &'static str {
    let any = |p: &[&str]| p.iter().any(|x| path == *x || path.starts_with(&format!("{x}/")));
    if path == "/timeline" || path == "/apis/unmatched" { "Timeline" }
    else if any(&["/inboxes/webhooks"]) { "Webhooks" }
    else if any(&["/notifications", "/inboxes", "/services/email", "/mailboxes"]) { "E-mail" }
    else if any(&["/streams"]) { "Streams" }
    else if any(&["/pubsub"]) { "Pub/Sub" }
    else if any(&["/kinesis"]) { "Kinesis" }
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
    else if any(&["/settings"]) { "Settings" }
    else { "" }
}

/// In use: a call today or in the last 7 days, or live now.
fn in_use(u: &Value) -> bool {
    api::n(u, "week") > 0 || u["live"].as_bool() == Some(true)
}

/// 2,140 → "2.1k".
fn short(n: i64) -> String {
    match n { 0 => String::new(), n if n < 1000 => n.to_string(), n if n < 10_000 => format!("{:.1}k", n as f64 / 1000.0), n if n < 1_000_000 => format!("{}k", n / 1000), n => format!("{:.1}M", n as f64 / 1e6) }
}

fn stored(key: &str) -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
}

/// whoami: { auth, who, email, currentEnvironment, mcp }
#[derive(Clone, Copy)]
pub struct Me(pub RwSignal<Value>);

impl Me {
    /// A person signed in on the screen (sign-in on).
    pub fn person(&self) -> Option<String> {
        let v = self.0.get();
        (v["auth"] == true && api::s(&v, "who").starts_with("person:")).then(|| api::s(&v, "email"))
    }
}

#[component]
fn Rail() -> impl IntoView {
    let me = expect_context::<Me>();
    let loc = use_location();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let unanswered = RwSignal::new(0i64);
    let usage = RwSignal::new(Value::Null);
    let clients = RwSignal::new(Vec::<Value>::new());
    // "in use" (default) or "all", per person.
    let show = RwSignal::new(stored(SHOW_KEY).filter(|v| v == "all").unwrap_or_else(|| "in use".into()));
    let expand = RwSignal::new(false);
    let last_usage = StoredValue::new(0f64);
    Effect::new(move |_| {
        live.tick.track();
        env.track();
        leptos::task::spawn_local(async move {
            let un = api::get("/apis/unmatched?since=1h").await.ok().and_then(|v| v["groups"].as_array().map(|g| g.iter().map(|x| api::n(x, "count")).sum::<i64>())).unwrap_or(0);
            unanswered.set(un);
            // Usage writes to the database: at most every 3 s however busy the live stream is.
            let now = js_sys::Date::now();
            if now - last_usage.get_value() > 3000.0 {
                last_usage.set_value(now);
                if let Ok(u) = api::get("/usage").await { usage.set(u); }
            }
            if let Ok(c) = api::get("/clients").await {
                clients.set(c["clients"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    let set_show = move |v: String| {
        if let Some(st) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) { let _ = st.set_item(SHOW_KEY, &v); }
        show.set(v);
    };
    let entry = move |label: &'static str, path: &'static str, kind: &'static str, used: bool| {
        let nav = nav.clone();
        let u = move || usage.get()[kind].clone();
        view! {
            <div class="item" class:on=move || rail_on(&loc.pathname.get()) == label class:off=path.is_empty() class:used=used && !kind.is_empty() class:unused=!used && !kind.is_empty()
                title=if path.is_empty() { "Not in this build yet" } else { "" }
                on:click=move |_| if !path.is_empty() { nav(path, Default::default()) }>
                <span class="name">{label}{move || (!kind.is_empty() && u()["live"].as_bool() == Some(true)).then(|| view! { <span class="dot" title="a call in the last 5 minutes"></span> })}</span>
                {move || match label {
                    "Timeline" => { let n = unanswered.get(); (n > 0).then(|| view! { <span class="badge" title="calls no route took, last hour">{format!("{n} not answered")}</span> }).into_any() }
                    _ if used && !kind.is_empty() => view! { <span class="count" title="calls today">{short(api::n(&u(), "today"))}</span> }.into_any(),
                    _ => ().into_any(),
                }}
            </div>
        }
    };
    view! {
        <nav class="rail">
            <div class="logo">"Stuntman"</div>
            <environments::Switcher/>
            {
                let entry = entry.clone();
                move || {
                    if env.get()["shared"].as_bool() == Some(true) && api::path_base().is_empty() {
                        // A shared Stuntman's root: no environment chosen, so only what is not per environment.
                        return view! {
                            {BOTTOM.iter().map(|(l, p, k)| entry(l, p, k, true)).collect_view()}
                            <div class="small cap" style="margin:14px 20px;line-height:16px">"Services, Timeline and Telemetry belong to one environment: open one to see them."</div>
                        }.into_any();
                    }
                    let u = usage.get();
                    let (mut on, off): (Vec<_>, Vec<_>) = KINDS.iter().partition(|(_, _, k)| in_use(&u[*k]));
                    on.sort_by_key(|(_, _, k)| -api::n(&u[*k], "week"));
                    let hidden = show.get() == "in use" && !expand.get();
                    let n_off = off.len();
                    view! {
                        {TOP.iter().map(|(l, p, k)| entry(l, p, k, k.is_empty() || in_use(&u[*k]))).collect_view()}
                        <div class="group row"><span class="caps grow">"Services"</span>
                            <ui::Seg options=vec![("in use", "In use"), ("all", "All")] value=show set=Callback::new(move |v: String| { expand.set(false); set_show(v) })/></div>
                        {on.iter().map(|(l, p, k)| entry(l, p, k, true)).collect_view()}
                        {(!on.is_empty() && n_off > 0).then(|| view! { <div class="sep-line"></div> })}
                        {if hidden {
                            (n_off > 0).then(|| view! { <div class="item unused" on:click=move |_| expand.set(true)>{format!("Not used yet  ·  {n_off}")}</div> }).into_any()
                        } else {
                            off.iter().map(|(l, p, k)| entry(l, p, k, false)).collect_view().into_any()
                        }}
                        <div class="rail-bottom">{BOTTOM.iter().map(|(l, p, k)| entry(l, p, k, true)).collect_view()}</div>
                    }.into_any()
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
            {move || me.person().map(|e| view! {
                <div class="me"><div class="row"><span class="grow" style="font-weight:500">{e.split('@').next().unwrap_or("").split(['.', '_', '-']).map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" ")}</span>
                    <a class="link small" href="/auth/logout">"Sign out"</a></div><div class="small cap">{e.clone()}</div></div> })}
            <div class="foot">{move || format!("Stuntman {}", api::s(&env.get(), "version"))}</div>
        </nav>
    }
}
