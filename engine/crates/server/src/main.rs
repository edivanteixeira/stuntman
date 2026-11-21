//! Stuntman server: wires service modules into one HTTP port (UI, /api, /mcp, provider intakes) plus
//! whatever ports the modules open themselves (SMTP…). Add a service by adding it to `modules()`.

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex, OnceLock},
};

use axum::{
    body::Bytes,
    extract::{Path, RawQuery, State},
    http::{HeaderMap, StatusCode},
    response::{sse, IntoResponse, Response, Sse},
    routing::{get, on, MethodFilter},
    Json, Router,
};
use stuntman_core::{callback, env, tools::snake, util, ApiError, Ctx, Module, Tool};
use futures::StreamExt;
use serde_json::{json, Map, Value};

fn modules() -> Vec<Box<dyn Module>> {
    let smtp_port = env("STUNTMAN_SMTP_PORT", "2525").parse().unwrap_or(2525);
    let hooks_port = env("STUNTMAN_HOOKS_PORT", "8027").parse().unwrap_or(8027);
    let otlp_http = env("STUNTMAN_OTLP_HTTP_PORT", "4318").parse().unwrap_or(4318);
    let otlp_grpc = env("STUNTMAN_OTLP_GRPC_PORT", "4317").parse().unwrap_or(4317);
    vec![
        Box::new(stuntman_email::Email { smtp_port, imap_port: env("STUNTMAN_IMAP_PORT", "1143").parse().unwrap_or(1143), pop3_port: env("STUNTMAN_POP3_PORT", "1110").parse().unwrap_or(1110) }),
        Box::new(stuntman_webhook::Webhook { port: hooks_port }),
        Box::new(stuntman_telemetry::Telemetry { http_port: otlp_http, grpc_port: otlp_grpc, gelf_port: env("STUNTMAN_GELF_PORT", "12201").parse().unwrap_or(12201), statsd_port: env("STUNTMAN_STATSD_PORT", "8125").parse().unwrap_or(8125) }),
        Box::new(stuntman_apis::Apis { port: env("STUNTMAN_APIS_PORT", "8028").parse().unwrap_or(8028), tls_port: env("STUNTMAN_APIS_TLS_PORT", "8443").parse().unwrap_or(8443) }),
        Box::new(stuntman_whatsapp::WhatsApp),
        Box::new(stuntman_sms::Sms),
        Box::new(stuntman_ai::Ai),
        Box::new(stuntman_events::Events),
        Box::new(stuntman_streams::Streams { kafka_port: env("STUNTMAN_KAFKA_PORT", "9092").parse().unwrap_or(9092), registry_port: env("STUNTMAN_SCHEMA_REGISTRY_PORT", "8081").parse().unwrap_or(8081) }),
        Box::new(stuntman_pubsub::PubSub { port: env("STUNTMAN_PUBSUB_PORT", "8085").parse().unwrap_or(8085) }),
        Box::new(stuntman_kinesis::Kinesis { port: env("STUNTMAN_KINESIS_PORT", "4567").parse().unwrap_or(4567) }),
        Box::new(stuntman_files::Files { ftp_port: env("STUNTMAN_FTP_PORT", "2121").parse().unwrap_or(2121), sftp_port: env("STUNTMAN_SFTP_PORT", "2222").parse().unwrap_or(2222) }),
        Box::new(stuntman_identity::Identity { ldap_port: env("STUNTMAN_LDAP_PORT", "3389").parse().unwrap_or(3389) }),
        Box::new(stuntman_keyvault::KeyVault { port: env("STUNTMAN_KEYVAULT_PORT", "8444").parse().unwrap_or(8444) }),
        Box::new(stuntman_messaging::Messaging { port: env("STUNTMAN_AMQP_PORT", "5673").parse().unwrap_or(5673), tls_port: env("STUNTMAN_AMQPS_PORT", "5671").parse().unwrap_or(5671) }),
        Box::new(stuntman_storage::Storage { blob_port: env("STUNTMAN_BLOB_PORT", "10000").parse().unwrap_or(10000), s3_port: env("STUNTMAN_S3_PORT", "9000").parse().unwrap_or(9000) }),
    ]
}

/// E-mail inboxes (E2) and webhook inboxes (H1–H2) share create_inbox, get_inbox and /inboxes in
/// the design. One tool answers for both: a name without "@" (or webhook keys, or kind="webhook")
/// goes to the webhook module, an address goes to e-mail.
fn shared_inbox_tools(mut tools: Vec<Tool>) -> Vec<Tool> {
    fn is_webhook(a: &Value, create: bool) -> bool {
        match a.get("kind").and_then(|v| v.as_str()) {
            Some("webhook") => return true,
            Some("email") => return false,
            _ => {}
        }
        // "address" is an e-mail word; a webhook inbox has a "name" (and names never hold "@").
        if a.get("address").is_some() || a.get("local_part").is_some() || a.get("localPart").is_some() {
            return false;
        }
        if let Some(s) = a.get("name").and_then(|v| v.as_str()) {
            return !s.contains('@');
        }
        create && ["expires", "mode", "fail", "then", "status", "traps", "secret", "delay", "hang", "scheme", "reject_bad", "rejectBad"].iter().any(|k| a.get(*k).is_some())
    }
    for (mail, hook, create) in [("create_inbox", "create_webhook_inbox", true), ("get_inbox", "get_webhook_inbox", false), ("delete_inbox", "remove_inbox", false)] {
        let (Some(e), Some(w)) = (tools.iter().find(|t| t.name == mail).cloned(), tools.iter().find(|t| t.name == hook).cloned()) else { continue };
        let (e2, w2) = (e.clone(), w.clone());
        let mut t = Tool::new(mail, e.description, move |ctx, a| {
            let (e, w) = (e2.clone(), w2.clone());
            async move {
                let v = Value::Object(a.0.clone());
                if is_webhook(&v, create) { w.call(ctx, v).await } else { e.call(ctx, v).await }
            }
        });
        t.description = Box::leak(format!("{} For a webhook inbox (a name without “@”, or kind=\"webhook\"): {}", e.description, w.description).into_boxed_str());
        t.params = e.params.iter().chain(w.params.iter().filter(|p| !e.params.iter().any(|q| q.0 == p.0))).map(|(n, d, _)| (*n, *d, false)).collect();
        t.params.push(("kind", "email | webhook, when the arguments do not say", false));
        t.routes = e.routes.clone();
        let i = tools.iter().position(|x| x.name == mail).unwrap();
        tools[i] = t;
    }
    // E3 and A5 both say get_service: a "service" argument means an HTTP API plugin.
    if let (Some(e), Some(ap)) = (tools.iter().find(|t| t.name == "get_service").cloned(), tools.iter().find(|t| t.name == "get_api_service").cloned()) {
        let (e2, a2) = (e.clone(), ap.clone());
        let mut t = Tool::new("get_service", e.description, move |ctx, a| {
            let (e, ap) = (e2.clone(), a2.clone());
            async move {
                let v = Value::Object(a.0.clone());
                if v.get("service").is_some() { ap.call(ctx, v).await } else { e.call(ctx, v).await }
            }
        });
        t.description = Box::leak(format!("{} For an HTTP API plugin (service=…): {}", e.description, ap.description).into_boxed_str());
        t.params = e.params.iter().chain(ap.params.iter().filter(|p| !e.params.iter().any(|q| q.0 == p.0))).map(|(n, d, _)| (*n, *d, false)).collect();
        t.routes = e.routes.clone();
        let i = tools.iter().position(|x| x.name == "get_service").unwrap();
        tools[i] = t;
    }
    // Webhook inboxes (H1) and storage subscriptions (S4) both say wait_for_delivery: a
    // "subscription" argument means storage.
    if let (Some(w), Some(st)) = (tools.iter().find(|t| t.name == "wait_for_delivery").cloned(), tools.iter().find(|t| t.name == "wait_for_storage_delivery").cloned()) {
        let (w2, s2) = (w.clone(), st.clone());
        let mut t = Tool::new("wait_for_delivery", w.description, move |ctx, a| {
            let (w, s) = (w2.clone(), s2.clone());
            async move {
                let v = Value::Object(a.0.clone());
                if v.get("subscription").is_some() { s.call(ctx, v).await } else { w.call(ctx, v).await }
            }
        });
        t.description = Box::leak(format!("{} For a storage subscription (subscription=…): {}", w.description, st.description).into_boxed_str());
        t.params = w.params.iter().chain(st.params.iter().filter(|p| !w.params.iter().any(|q| q.0 == p.0))).map(|(n, d, _)| (*n, *d, false)).collect();
        t.routes = w.routes.clone();
        let i = tools.iter().position(|x| x.name == "wait_for_delivery").unwrap();
        tools[i] = t;
    }
    tools
}

/// Messaging (M2, M3) uses the rule editor (S2) and the account API (S5) of Storage, by the same
/// names: service = azure-service-bus | rabbitmq goes to messaging_*. Its "next delivery" also
/// shares set_next_delivery with storage events: a namespace argument means messaging.
fn shared_messaging_tools(mut tools: Vec<Tool>) -> Vec<Tool> {
    let pairs = ["list_rule_options", "list_rules", "test_rule", "create_rule", "update_rule", "set_rule_enabled", "move_rule", "delete_rule", "put_rules_yaml",
        "get_service_schema", "list_service_accounts", "get_account", "create_account", "update_account", "rotate_key", "delete_account"];
    for shared in pairs {
        let own = format!("messaging_{shared}");
        let (Some(s), Some(m)) = (tools.iter().find(|t| t.name == shared).cloned(), tools.iter().find(|t| t.name == own).cloned()) else { continue };
        let (s2, m2) = (s.clone(), m.clone());
        let mut t = Tool::new(shared, s.description, move |ctx, a| {
            let (s, m) = (s2.clone(), m2.clone());
            async move {
                let v = Value::Object(a.0.clone());
                if stuntman_messaging::tools::is_messaging(&v) { m.call(ctx, v).await } else { s.call(ctx, v).await }
            }
        });
        t.description = Box::leak(format!("{} Also for messaging: service = azure-service-bus | rabbitmq (accounts are namespaces).", s.description).into_boxed_str());
        t.params = s.params.iter().map(|(n, d, r)| (*n, if *n == "service" { "azure-blob | s3 | azure-service-bus | rabbitmq" } else { *d }, *r && *n != "account")).collect();
        if !t.params.iter().any(|p| p.0 == "namespace") && shared.contains("account") {
            t.params.push(("namespace", "messaging: the namespace (same as account)", false));
        }
        t.routes = s.routes.clone();
        let i = tools.iter().position(|x| x.name == shared).unwrap();
        tools[i] = t;
    }
    for (shared, own) in [("set_next_delivery", "set_next_message_delivery"), ("wait_for_message", "wait_for_queue_message")] {
        let (Some(s), Some(m)) = (tools.iter().find(|t| t.name == shared).cloned(), tools.iter().find(|t| t.name == own).cloned()) else { continue };
        let (s2, m2) = (s.clone(), m.clone());
        let mut t = Tool::new(shared, s.description, move |ctx, a| {
            let (s, m) = (s2.clone(), m2.clone());
            async move {
                let v = Value::Object(a.0.clone());
                if v.get("namespace").is_some() { m.call(ctx, v).await } else { s.call(ctx, v).await }
            }
        });
        t.description = Box::leak(format!("{} For a queue or topic (namespace=…): {}", s.description, m.description).into_boxed_str());
        t.params = s.params.iter().chain(m.params.iter().filter(|p| !s.params.iter().any(|q| q.0 == p.0))).map(|(n, d, _)| (*n, *d, false)).collect();
        t.routes = s.routes.iter().chain(m.routes.iter()).cloned().collect();
        let i = tools.iter().position(|x| x.name == shared).unwrap();
        tools[i] = t;
    }
    tools.retain(|t| !t.name.starts_with("messaging_") && t.name != "set_next_message_delivery" && t.name != "wait_for_queue_message");
    tools
}

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

mod auth;
mod environments;
mod timeline;

/// The process: every environment, the tools, the shared resources.
pub struct Srv {
    pub envs: env::Envs,
    pub tools: Arc<Vec<Tool>>,
    pub mods: Vec<Box<dyn Module>>,
    /// The data folder; environments live in environments/<name>/.
    pub root: std::path::PathBuf,
    pub port: u16,
    pub host: String,
    pub http: reqwest::Client,
    pub signer: Arc<callback::Signer>,
    web: std::path::PathBuf,
    routers: Mutex<HashMap<String, Router>>,
    /// MCP session id → client name, for "by claude-code via MCP".
    sessions: Mutex<HashMap<String, String>>,
    /// tool name → the module that owns it (the Timeline lane of an agent's action).
    owner: HashMap<&'static str, &'static str>,
}

static SRV: OnceLock<Srv> = OnceLock::new();

pub fn srv() -> &'static Srv {
    SRV.get().expect("server started")
}

#[derive(Clone)]
struct App {
    ctx: Ctx,
}

#[tokio::main]
async fn main() {
    // The project was called Dublê: DUBLE_* variables still work when the STUNTMAN_* one is unset.
    for (k, v) in std::env::vars() {
        if let Some(rest) = k.strip_prefix("DUBLE_") {
            let new = format!("STUNTMAN_{rest}");
            if std::env::var_os(&new).is_none() {
                std::env::set_var(new, v);
            }
        }
    }
    let root = std::path::PathBuf::from(env("STUNTMAN_DATA", "data"));
    std::fs::create_dir_all(&root).expect("data folder");
    let port: u16 = env("STUNTMAN_PORT", "8025").parse().unwrap_or(8025);
    let first = env("STUNTMAN_ENV", "staging");
    environments::migrate_flat(&root, &first);
    let mods = modules();
    let mut tools = vec![];
    let mut owner = HashMap::new();
    for m in &mods {
        for t in m.tools() {
            owner.insert(t.name, m.id());
            tools.push(t);
        }
    }
    tools.extend(environments::tools());
    tools.extend(timeline::tools());
    tools.extend(auth::tools());
    let tools = shared_messaging_tools(shared_inbox_tools(tools));
    let mut seen = std::collections::HashSet::new();
    for t in &tools {
        assert!(seen.insert(t.name), "two modules define the tool {}", t.name);
    }
    let _ = SRV.set(Srv {
        envs: env::Envs::default(),
        tools: Arc::new(tools),
        mods,
        signer: Arc::new(callback::Signer::load_or_create(&root.join("keys/events-ecdsa.pem")).expect("events key")),
        root: root.clone(),
        port,
        host: env("STUNTMAN_HOST", &format!("localhost:{port}")),
        http: reqwest_client(),
        web: std::path::PathBuf::from(env("STUNTMAN_WEB", "web/dist")),
        routers: Default::default(),
        sessions: Default::default(),
        owner,
    });
    let s = srv();
    // The default environment opens first (it answers plain localhost), then the others.
    let mut names: Vec<String> = std::fs::read_dir(root.join("environments")).into_iter().flatten().flatten().filter(|e| e.path().is_dir()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
    names.sort();
    if !names.contains(&first) {
        if names.is_empty() {
            names.push(first.clone());
        }
    }
    let default = if names.contains(&first) { first.clone() } else { names[0].clone() };
    s.envs.set_default(&default);
    names.retain(|n| *n != default);
    names.insert(0, default);
    for n in &names {
        environments::open(n).unwrap_or_else(|e| panic!("environment {n}: {}", e.message));
    }
    for m in &s.mods {
        m.listen(s.envs.clone());
    }
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            environments::sweep().await;
        }
    });
    // Ctrl-C or a container stop: write the usage counts the heartbeat has not written yet.
    tokio::spawn(async {
        #[cfg(unix)]
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
        #[cfg(unix)]
        tokio::select! { _ = tokio::signal::ctrl_c() => {} _ = term.recv() => {} }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        for c in srv().envs.all() {
            let _ = stuntman_core::usage::flush(&c).await;
        }
        std::process::exit(0);
    });

    // Sign-in, the central MCP's OAuth and its metadata live on the root, before any environment.
    let root_router = auth::routes().fallback(dispatch);
    if auth::on() {
        println!("Sign-in on: {} · provider routes and /e/<name>/login stay open", auth::conf().unwrap().issuer);
    }
    println!("Stuntman {} · environments {} · http://localhost:{port} · MCP http://localhost:{port}/mcp (others: /e/<name>/mcp) · data {}", env!("CARGO_PKG_VERSION"), names.join(", "), root.display());
    // Connection by connection (as the provider ports): a forced SMS or WhatsApp failure can reset
    // or close the socket the app sent on. serve returns only when the port cannot be bound.
    static BIND: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    // The same over HTTPS (Test CA): for clients that take only an https authority, such as MSAL.
    let https = env("STUNTMAN_HTTPS_PORT", "8445").parse().unwrap_or(8445u16);
    if https > 0 {
        tokio::spawn(stuntman_apis::tls::serve_main(https, root_router.clone()));
    }
    stuntman_core::netserve::serve(port, root_router, "http", &BIND).await;
    panic!("http port: {}", BIND.lock().unwrap().clone().unwrap_or_default());
}

/// Everything one environment answers on the main port: /api, /mcp, provider intakes, the UI.
pub fn env_router(ctx: Ctx) -> Router {
    let s = srv();
    let mut api: Router<App> = Router::new();
    for t in s.tools.iter() {
        for r in &t.routes {
            let name = t.name;
            let filter = match r.method {
                "GET" => MethodFilter::GET,
                "POST" => MethodFilter::POST,
                "PUT" => MethodFilter::PUT,
                "PATCH" => MethodFilter::PATCH,
                "DELETE" => MethodFilter::DELETE,
                m => panic!("method {m}"),
            };
            api = api.route(
                r.path,
                on(filter, move |s: State<App>, p: Path<HashMap<String, String>>, q: RawQuery, h: HeaderMap, b: Bytes| http_tool(s, name, p, q, h, b)),
            );
        }
    }
    api = api
        .route("/stream", get(stream))
        .route("/clients", get(|s: State<App>| async move { Json(json!({"clients": s.ctx.live.clients()})) }))
        .route("/tools", get(|| async move { Json(catalog(&srv().tools)) }))
        // An app's KEY=VALUE lines (environment.yaml apps.<app>): `curl …/api/env/worker > .env`.
        .route("/env/{app}", get(|s: State<App>, Path(app): Path<String>| async move {
            match environments::app_env(&s.ctx, &app).await {
                Ok(v) => ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], v["env"].as_str().unwrap_or("").to_string()).into_response(),
                Err(e) => (StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST), format!("{}\n", e.message)).into_response(),
            }
        }))
        .route("/env", get(|s: State<App>| async move {
            let f = s.ctx.state.file.read().unwrap().clone();
            Json(json!({"env": &*s.ctx.env, "host": &*s.ctx.host, "prefix": s.ctx.prefix(), "paused": f.paused, "pausedSince": f.paused_since,
                "lifetime": f.lifetime, "expiresAt": f.expires_at, "default": srv().envs.default_name(), "version": env!("CARGO_PKG_VERSION"),
                "shared": stuntman_core::shared_public_url().is_some(), "publicUrl": s.ctx.public_base()}))
        }));
    let mut extra: Router<Ctx> = Router::new();
    for m in &s.mods {
        extra = extra.merge(m.extra_routes());
    }
    let index = s.web.join("index.html");
    let svc = tower_http::services::ServeDir::new(&s.web).fallback(tower_http::services::ServeFile::new(index));
    Router::new()
        .nest("/api", api)
        .route("/mcp", axum::routing::post(mcp).get(|| async { (StatusCode::METHOD_NOT_ALLOWED, "POST JSON-RPC here") }))
        .route("/docs", axum::routing::get(|| docs(String::new())))
        .route("/docs/{*path}", axum::routing::get(|axum::extract::Path(p): axum::extract::Path<String>| docs(p)))
        .with_state(App { ctx: ctx.clone() })
        .merge(extra.with_state(ctx))
        .fallback_service(svc)
}

/// The documentation of this very version, as Markdown: /docs is the index, /docs/services/rabbitmq.md
/// one service. Agents read it from here (the MCP instructions point at it). STUNTMAN_DOCS: the folder.
async fn docs(path: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    let rel = if path.is_empty() || path == "/" { "README.md".to_string() } else { path.trim_start_matches('/').to_string() };
    let safe = rel.ends_with(".md") && std::path::Path::new(&rel).components().all(|c| matches!(c, std::path::Component::Normal(_)));
    let base = std::path::PathBuf::from(env("STUNTMAN_DOCS", "../docs"));
    match std::fs::read_to_string(base.join(&rel)).ok().filter(|_| safe) {
        Some(t) => ([("content-type", "text/markdown; charset=utf-8")], t).into_response(),
        None => (StatusCode::NOT_FOUND, [("content-type", "text/plain; charset=utf-8")],
            format!("No {rel} here. The index is /docs; this version's docs are also at https://github.com/edivanteixeira/stuntman/tree/v{}/docs\n", env!("CARGO_PKG_VERSION"))).into_response(),
    }
}

/// Finds the request's environment (/e/<name>/ prefix, else host name, else the default one) and
/// hands the request to its router. A paused environment's provider intakes answer 503; its
/// screens, API and MCP keep working so it can be looked at and resumed.
async fn dispatch(req: axum::extract::Request) -> Response {
    use tower::ServiceExt;
    let s = srv();
    let path = req.uri().path().to_string();
    // A proxy (an Ingress) sends the public host in X-Forwarded-Host and keeps or rewrites Host.
    let (fwd, plain, proto) = {
        let hv = |k: &str| req.headers().get(k).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim().to_string()).filter(|v| !v.is_empty());
        (hv("x-forwarded-host"), hv("host"), hv("x-forwarded-proto").unwrap_or("http".into()))
    };
    let host = fwd.or(plain.clone()).unwrap_or(format!("localhost:{}", s.port));
    let (ctx, rest) = match path.strip_prefix("/e/") {
        Some(r) => {
            let (name, tail) = r.split_once('/').unwrap_or((r, ""));
            (s.envs.get(name), Some(format!("/{tail}")))
        }
        None => (s.envs.by_host(Some(&host)).or_else(|| s.envs.by_host(plain.as_deref())), None),
    };
    let Some(ctx) = ctx else {
        return ApiError::not_found("No Stuntman environment answers here").with(json!({"environments": s.envs.all().iter().map(|c| c.env.to_string()).collect::<Vec<_>>()})).into_response();
    };
    let mut req = req;
    // Only this server says who a request is: drop what a caller sent.
    req.headers_mut().remove("x-stuntman-who");
    req.headers_mut().remove("x-stuntman-central");
    {
        let p = rest.as_deref().unwrap_or(&path).split('?').next().unwrap_or("").to_string();
        let full = req.uri().path_and_query().map(|x| x.to_string()).unwrap_or(path.clone());
        match auth::gate(req.headers(), req.method().as_str(), &p, &ctx.env, rest.is_some(), &full) {
            Ok(Some(w)) => { req.headers_mut().insert("x-stuntman-who", w.parse().unwrap()); }
            Ok(None) => {}
            Err(r) => return r,
        }
        // The central MCP: one address for everyone on a shared server; each session picks its environment.
        if rest.is_none() && p == "/mcp" && (auth::on() || stuntman_core::shared_public_url().is_some()) {
            req.headers_mut().insert("x-stuntman-central", "1".parse().unwrap());
        }
    }
    if let Some(p) = &rest {
        let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        *req.uri_mut() = format!("{p}{q}").parse().unwrap_or_default();
    }
    let p = rest.as_deref().unwrap_or(&path);
    if ctx.state.paused() && p.starts_with("/sendgrid") {
        return ApiError::new(503, format!("{} is paused in Stuntman", ctx.env)).into_response();
    }
    let Some(router) = s.routers.lock().unwrap().get(&*ctx.env).cloned() else {
        return ApiError::new(503, "This environment is starting").into_response();
    };
    let base = (format!("{proto}://{host}"), ctx.env.to_string(), rest.is_some());
    stuntman_core::REQUEST_BASE.scope(base, router.oneshot(req)).await.into_response()
}

fn reqwest_client() -> reqwest::Client {
    reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build().unwrap()
}

fn who(h: &HeaderMap) -> String {
    match h.get("x-stuntman-client").or(h.get("x-duble-client")).and_then(|v| v.to_str().ok()) {
        Some("ui") => "you".into(),
        Some(c) => format!("api:{c}"),
        None => format!("api:{}", h.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("client").split('/').next().unwrap_or("client")),
    }
}

async fn http_tool(State(app): State<App>, name: &'static str, Path(p): Path<HashMap<String, String>>, RawQuery(q): RawQuery, h: HeaderMap, body: Bytes) -> Response {
    let mut args = Map::new();
    if !body.is_empty() {
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(o)) => args.extend(o),
            Ok(_) => return ApiError::bad("The body must be a JSON object").into_response(),
            Err(e) => return ApiError::bad(format!("Body is not JSON: {e}")).into_response(),
        }
    }
    if let Some(q) = q {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(&q).unwrap_or_default();
        let mut multi: HashMap<String, Vec<String>> = HashMap::new();
        for (k, v) in pairs {
            multi.entry(k).or_default().push(v);
        }
        for (k, v) in multi {
            args.insert(k, json!(v.join(",")));
        }
    }
    for (k, v) in p {
        args.insert(k, json!(v));
    }
    let by = who(&h);
    if by != "you" {
        app.ctx.live.seen(by.trim_start_matches("api:"), "API");
    }
    args.insert("_by".into(), json!(by));
    if let Some(w) = h.get("x-stuntman-who").and_then(|v| v.to_str().ok()) {
        args.insert("_who".into(), json!(w));
    }
    let tool = srv().tools.iter().find(|t| t.name == name).unwrap();
    let a = Value::Object(args);
    let r = tool.call(app.ctx.clone(), a.clone()).await;
    acted(&app.ctx, tool, &by, &a, r.as_ref().err()).await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

/// Mutating calls (any non-GET route) by agents and tests go on the Timeline; your own clicks
/// already show as the rows the services write.
async fn acted(ctx: &Ctx, tool: &Tool, by: &str, args: &Value, err: Option<&ApiError>) {
    if by == "you" || tool.routes.iter().all(|r| r.method == "GET") {
        return;
    }
    // Shared tools (rules, accounts, next delivery) belong to messaging when they name it.
    let module = if stuntman_messaging::tools::is_messaging(args) || args.get("namespace").is_some() { "messaging" } else { srv().owner.get(tool.name).copied().unwrap_or("environments") };
    timeline::act(ctx, module, tool.name, by, args, err.map(|e| e.message.as_str())).await;
}

fn catalog(tools: &[Tool]) -> Value {
    json!(tools
        .iter()
        .map(|t| json!({"name": t.name, "description": t.description, "params": t.params.iter().map(|(n, d, r)| json!({"name": n, "description": d, "required": r})).collect::<Vec<_>>(),
            "http": t.routes.iter().map(|r| format!("{} /api{}", r.method, r.path)).collect::<Vec<_>>()}))
        .collect::<Vec<_>>())
}

async fn stream(State(app): State<App>) -> Sse<impl futures::Stream<Item = Result<sse::Event, Infallible>>> {
    let rx = tokio_stream::wrappers::BroadcastStream::new(app.ctx.live.subscribe());
    let s = rx.filter_map(|c| async move { c.ok().map(|c| Ok(sse::Event::default().event("change").json_data(c).unwrap())) });
    Sse::new(s).keep_alive(sse::KeepAlive::default())
}

/// MCP over streamable HTTP, JSON responses. Every tool of every module, same names as HTTP.
async fn mcp(State(app): State<App>, h: HeaderMap, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "Parse error"}}))).into_response();
    };
    if let Value::Array(batch) = req {
        let mut out = vec![];
        for r in batch {
            if let Some(v) = mcp_one(&app, &h, r).await.0 {
                out.push(v);
            }
        }
        return Json(out).into_response();
    }
    let (v, session) = mcp_one(&app, &h, req).await;
    match v {
        None => StatusCode::ACCEPTED.into_response(),
        Some(v) => {
            let mut r = Json(v).into_response();
            if let Some(s) = session {
                r.headers_mut().insert("mcp-session-id", s.parse().unwrap());
            }
            r
        }
    }
}

/// MCP session id → the environment the session picked with use_environment (central MCP).
fn session_envs() -> &'static Mutex<HashMap<String, String>> {
    static M: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn env_names() -> String {
    srv().envs.all().iter().map(|c| c.env.to_string()).collect::<Vec<_>>().join(", ")
}

async fn mcp_one(app: &App, h: &HeaderMap, req: Value) -> (Option<Value>, Option<String>) {
    let id = req.get("id").cloned();
    let method = req["method"].as_str().unwrap_or("");
    let session = h.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
    let client = session.as_ref().and_then(|s| srv().sessions.lock().unwrap().get(s).cloned()).unwrap_or("agent".into());
    let who = h.get("x-stuntman-who").and_then(|v| v.to_str().ok()).map(String::from);
    let central = h.get("x-stuntman-central").is_some();
    let ok = |r: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": r}));
    let err = |code: i64, m: String| Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}}));
    let text = |t: String, is_error: bool| Some(json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": t}], "isError": is_error}}));
    match method {
        "initialize" => {
            let name = req["params"]["clientInfo"]["name"].as_str().unwrap_or("agent").to_string();
            let sid = util::id("mcp");
            srv().sessions.lock().unwrap().insert(sid.clone(), name.clone());
            app.ctx.live.seen(&name, "MCP");
            let pv = req["params"]["protocolVersion"].as_str().unwrap_or("2025-03-26").to_string();
            let docs = " Unsure what Stuntman can do or how? GET /docs on this server (Markdown, this version): the index, with one file per service listing every endpoint, tool, behaviour and provocation.";
            let upstream = " When Stuntman itself falls short (a provider behaviour it gets wrong, a missing endpoint or tool, a bug), tell the developer the fix you would make and offer to send it as a pull request to https://github.com/edivanteixeira/stuntman; only after a yes, with nothing from their project in it.";
            let instructions = if central {
                format!("Stuntman stands in for external providers. This is the central MCP of a shared Stuntman: each call works in one environment ({}). \
                    Pick it with use_environment, or pass environment on a tool; every answer starts with [name], the environment it used.{docs}{upstream}", env_names())
            } else {
                format!("Stuntman stands in for external providers in environment {}. Tools act on this environment only; another environment has its own MCP at /e/<name>/mcp (list_environments shows them).{docs}{upstream}", app.ctx.env)
            };
            (ok(json!({"protocolVersion": pv, "capabilities": {"tools": {"listChanged": false}}, "serverInfo": {"name": "stuntman", "version": env!("CARGO_PKG_VERSION")}, "instructions": instructions})), Some(sid))
        }
        m if m.starts_with("notifications/") => (None, None),
        "ping" => (ok(json!({})), None),
        "tools/list" => {
            let mut tools: Vec<Value> = srv().tools.iter().map(|t| {
                let mut schema = t.schema();
                if central {
                    schema["properties"]["environment"] = json!({"type": "string", "description": "the environment this call works in (default: the session's, see use_environment)"});
                }
                json!({"name": t.name, "description": t.description, "inputSchema": schema})
            }).collect();
            if central {
                tools.insert(0, json!({"name": "use_environment", "description": "This MCP session works in that environment from now on (list_environments shows them).",
                    "inputSchema": {"type": "object", "properties": {"name": {"type": "string", "description": "environment name"}}, "required": ["name"]}}));
            }
            (ok(json!({"tools": tools})), None)
        }
        "tools/call" => {
            app.ctx.live.seen(&client, "MCP");
            let name = req["params"]["name"].as_str().unwrap_or("");
            let mut args = match req["params"]["arguments"].clone() {
                Value::Object(o) => o,
                _ => Map::new(),
            };
            args = args.into_iter().map(|(k, v)| (snake(&k), v)).collect();
            if central && name == "use_environment" {
                let n = args.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if srv().envs.get(&n).is_none() {
                    return (text(format!("No environment “{n}”. Pick one of: {}", env_names()), true), None);
                }
                if let Some(s) = &session { session_envs().lock().unwrap().insert(s.clone(), n.clone()); }
                return (text(format!("[{n}] this session works in {n} from now on"), false), None);
            }
            let Some(t) = srv().tools.iter().find(|t| t.name == name) else {
                return (err(-32602, format!("Unknown tool {name}")), None);
            };
            // Which environment: the argument, else the session's, else the person's (Use in MCP); no silent default.
            let ctx = if central {
                let asked = args.remove("environment").and_then(|v| v.as_str().map(String::from));
                let from_session = session.as_ref().and_then(|s| session_envs().lock().unwrap().get(s).cloned());
                let person = who.as_deref().and_then(|w| w.split_once(':')).filter(|(k, _)| *k == "person" || *k == "agent").and_then(|(_, e)| auth::current_env(e));
                let pick = asked.clone().or(from_session.clone()).or(person);
                match pick.as_deref().and_then(|n| srv().envs.get(n)) {
                    Some(c) => c,
                    None => {
                        if let (Some(s), Some(_)) = (&session, &from_session) { session_envs().lock().unwrap().remove(s); }
                        let gone = pick.map(|p| format!("[{p}] no longer exists. ")).unwrap_or_default();
                        return (text(format!("{gone}Pick an environment: {}. Call use_environment(\"<name>\"), or pass environment on the tool.", env_names()), true), None);
                    }
                }
            } else {
                app.ctx.clone()
            };
            if central && auth::on() && matches!(name, "create_environment" | "delete_environment") && who.as_deref() != Some("admin") {
                return (text(format!("[{}] {name} needs the admin key: people make and delete environments in the pipeline", ctx.env), true), None);
            }
            let by = format!("mcp:{client}");
            args.insert("_by".into(), json!(by));
            if let Some(w) = &who { args.insert("_who".into(), json!(w)); }
            let a = Value::Object(args);
            let r = t.call(ctx.clone(), a.clone()).await;
            acted(&ctx, t, &by, &a, r.as_ref().err()).await;
            let (body, is_error) = match r {
                Ok(v) => (serde_json::to_string_pretty(&v).unwrap(), false),
                Err(e) => (serde_json::to_string_pretty(&json!({"error": e.message, "status": e.status, "detail": e.detail})).unwrap(), true),
            };
            (text(if central { format!("[{}] {body}", ctx.env) } else { body }, is_error), None)
        }
        _ => (err(-32601, format!("Method not found: {method}")), None),
    }
}
