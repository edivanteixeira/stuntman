//! APIs tools (A1–A5 "API & MCP" sheets). The plugin file is the source of truth: the screens,
//! the API and agents change the same YAML.

use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};
use serde_json::{json, Value};

use crate::{
    engine,
    plugin::{self, Answer, Decide, Plugin, Route},
    store,
};

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

fn since(a: &Args, d: &str) -> Result<i64, ApiError> {
    let s = a.str("since").unwrap_or(d.into());
    let ms = util::parse_ms(&s).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d"))?;
    Ok(util::now_ms() - ms as i64)
}

fn service_of(a: &Args) -> Result<String, ApiError> {
    a.str("service").ok_or_else(|| ApiError::bad("“service” is required, e.g. serpro-datavalid"))
}

/// The left column of A1 and the header of A3–A5: a service with its counts and state.
pub async fn service_view(ctx: &Ctx, p: &Plugin) -> Value {
    let counts = store::counts(ctx, util::now_ms() - 86_400_000).await.unwrap_or_default();
    service_view_with(ctx, p, &counts).await
}

/// `counts`: calls and billable calls today by service, read once for a list of services.
async fn service_view_with(ctx: &Ctx, p: &Plugin, counts: &std::collections::BTreeMap<String, (i64, i64)>) -> Value {
    let (calls, billable) = counts.get(&p.service.name).cloned().unwrap_or((0, 0));
    let warn = plugin::per(&p.service.billing.warn_above).map(|x| x.0 as i64);
    let forced = store::forced(ctx, &p.service.name).await.ok().flatten();
    let host = ctx.host.split(':').next().unwrap_or("localhost").to_string();
    let ip = if host == "localhost" { "127.0.0.1".to_string() } else { host };
    let base = ctx.state.file.read().unwrap().base_domain.clone();
    json!({
        "name": p.service.name, "title": if p.service.title.is_empty() { p.service.name.clone() } else { p.service.title.clone() }, "origin": p.service.origin,
        "region": p.service.region, "description": p.service.description, "billable": p.service.billing.enabled && p.routes.iter().chain(p.operations.iter()).any(|r| r.billable),
        "callsToday": calls, "billableToday": billable, "costToday": (billable as f64) * p.service.billing.per_call,
        "warnAbove": warn, "overWarn": warn.map(|w| p.service.billing.enabled && billable > w).unwrap_or(false),
        "forced": forced, "forceLog": store::force_log(ctx, Some(p.service.name.clone()), 10).await.unwrap_or_default(), "routes": p.routes.len(),
        "soap": p.soap.as_ref().map(|s| json!({"version": s.version, "endpoint": s.endpoint, "wsdl": s.wsdl})), "operations": p.operations.len(),
        "spec": p.spec.as_ref().map(|s| json!({"version": s.version, "file": s.openapi, "apiVersion": s.api_version, "source": s.source, "validate": s.validate, "schemas": s.schemas.len()})),
        "record": p.record.as_ref().map(|r| json!({"upstream": r.upstream, "mode": r.mode, "until": r.until, "stopped": r.stopped, "paused": r.paused, "live": crate::record::live(r), "started": r.started})), "recorded": match &p.record { Some(r) => store::count_recorded(ctx, &p.service.name, r.started).await.unwrap_or(0), None => 0 }, "answers": p.answers.len(), "pages": p.pages.len(),
        "mtls": p.service.auth.client_certificate.as_ref().map(|c| c.required).unwrap_or(false), "session": p.service.auth.session,
        "mcp": p.mcp.as_ref().map(|m| json!({"path": m.path, "tools": crate::mcp::tools(p), "auth": m.auth, "sse": m.sse})),
        "signature": p.soap.as_ref().and_then(|s| s.signature.clone()), "icp": p.service.auth.client_certificate.as_ref().map(|c| c.icp).unwrap_or(false),
        "urls": {"local": format!("http://{ip}:{}{}/{}", crate::port(), ctx.prefix(), p.service.name), "host": format!("http://{}.{base}:{}", p.service.name, crate::port()),
                 "real": p.service.real_host, "realBase": p.service.base_url,
                 "https": format!("https://{}:{}", if p.service.real_host.is_empty() { format!("{}.{base}", p.service.name) } else { p.service.real_host.first().to_string() }, crate::tls_port())},
    })
}

fn draft_route_from(u: &Value, plugins: &[Plugin]) -> Route {
    let method = u["method"].as_str().unwrap_or("GET").to_string();
    let path = u["path"].as_str().unwrap_or("/").to_string();
    let body: Value = serde_json::from_str(u["request"]["body"].as_str().unwrap_or("")).unwrap_or(Value::Null);
    let fields: Vec<String> = body.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
    // Steps of the closest route are copied, so the plugin's answers can be reused.
    let base = u["closest"]["route"].as_str().and_then(|cr| plugins.iter().find(|p| p.service.name == u["closest"]["service"].as_str().unwrap_or("")).and_then(|p| p.routes.iter().find(|r| r.route == cr))).cloned();
    match base {
        Some(mut r) => {
            r.route = format!("{method} {path}");
            if r.receive.body.is_empty() {
                r.receive.body = fields;
            }
            r
        }
        None => Route { route: format!("{method} {path}"), receive: plugin::Receive { body: fields, ..Default::default() }, decide: vec![Decide { when: None, answer: "ok".into(), ..Default::default() }], ..Default::default() },
    }
}

pub fn all() -> Vec<Tool> {
    let mut v = calls();
    v.extend(catalog_tools());
    v.extend(step_tools());
    v.extend(unmatched_tools());
    v.extend(plugin_tools());
    v.extend(answers_tools());
    v.extend(service_tools());
    v.extend(crate::restpki::tools());
    v.extend(crate::soap::tools());
    v.extend(crate::openapi::tools());
    v.extend(crate::mocks::tools());
    v.extend(crate::certs::tools());
    v.extend(crate::record::tools());
    v
}

// ---------------------------------------------------------------- A1 calls

fn calls() -> Vec<Tool> {
    vec![
        Tool::new("list_api_services", "HTTP API services (plugins) in this environment: origin, region, calls and billable calls today, forced answer, URLs; broken files with their error.", |ctx, _a| async move {
            let counts = store::counts(&ctx, util::now_ms() - 86_400_000).await.unwrap_or_default();
            let mut out = vec![];
            for p in plugin::shared(&ctx).iter() {
                out.push(service_view_with(&ctx, p, &counts).await);
            }
            let broken: Vec<Value> = plugin::broken(&ctx).into_iter().map(|(n, e)| json!({"name": n, "error": e, "file": plugin::rel(&n)})).collect();
            Ok(json!({"services": out, "broken": broken, "port": crate::port()}))
        })
        .route("GET", "/apis/services"),
        Tool::new("list_api_calls", "Calls any HTTP plugin answered, newest first: sentence, answer name, status, billable, trace id.", |ctx, a| async move {
            let s = since(&a, "15m")?;
            let mut v = store::list(&ctx, a.str("service"), a.str("answer"), s, a.u64("limit").unwrap_or(200) as i64).await?;
            if let Some(q) = a.str("q") {
                let q = q.to_lowercase();
                let digits: String = q.chars().filter(|c| c.is_ascii_digit()).collect();
                v.retain(|c| c.to_string().to_lowercase().contains(&q) || (digits.len() >= 3 && c["sent"].to_string().chars().filter(|x| x.is_ascii_digit()).collect::<String>().contains(&digits)));
            }
            Ok(json!({"calls": v}))
        })
        .param("service", "plugin name").param("answer", "answer name").param("since", "15m (default), 1h, 1d").param("q", "path, id, CPF…").param("limit", "default 200")
        .route("GET", "/apis/calls"),
        Tool::new("list_api_traffic", "Every call in the order it came, answered by a route or not (matched: false, answered 404 \"no route\"), after a cursor: pass the last `next` as `after` to get only what came since. One cursor for both lists, so a test reads the calls its step made without cutting by time.", |ctx, a| async move {
            let after = a.u64("after").unwrap_or(0) as i64;
            let calls = store::traffic(&ctx, after, a.u64("limit").unwrap_or(500).min(5000) as i64).await?;
            let next = calls.last().and_then(|c| c["seq"].as_i64()).unwrap_or(after);
            Ok(json!({"after": after, "next": next, "count": calls.len(), "calls": calls}))
        })
        .param("after", "seq of the last call already read (default 0: from the start); use the `next` of the previous answer").param("limit", "default 500, at most 5000")
        .route("GET", "/apis/traffic"),
        Tool::new("get_api_call", "What the app sent and what Stuntman answered, the steps and webhooks; raw=true adds headers and bodies.", |ctx, a| async move {
            store::get(&ctx, &a.need("id")?, a.bool("raw").unwrap_or(false)).await
        })
        .req("id", "call id").param("raw", "true: headers and bodies")
        .route("GET", "/apis/calls/{id}"),
        Tool::new("wait_for_api_call", "Waits for the next call to a service (and route, and answer): what a test waits for after acting in the app.", |ctx, a| async move {
            let svc = service_of(&a)?;
            let start = a.get("since").and_then(|v| v.as_i64()).unwrap_or(util::now_ms() - 2000);
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            loop {
                let v = store::list(&ctx, Some(svc.clone()), a.str("answer"), start, 50).await?;
                if let Some(c) = v.iter().rev().find(|c| a.str("route").map(|r| c["route"].as_str().unwrap_or("").contains(&r)).unwrap_or(true)) {
                    return Ok(c.clone());
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No call to {svc} yet")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .req("service", "plugin").param("route", "part of “METHOD /path”").param("answer", "answer name").param("timeout", "default 30s").param("since", "ms; default 2 s ago")
        .route("GET", "/apis/calls/wait"),
        Tool::new("force_answer", "The next calls get this named answer, or a generic one (reset, empty, timeout, 401, 403, 429, 500, 503, 503_html, html; cut and slow for streams; session_expired for MCP): count=N calls or for=10m; route= limits it to one route (the token route is forced only this way); answer=default stops.", |ctx, a| async move {
            let svc = service_of(&a)?;
            let p = plugin::get(&ctx, &svc)?;
            let ans = a.need("answer")?;
            if ans != "default" && !p.answers.iter().any(|x| x.name == ans) && !plugin::GENERIC.iter().any(|g| g.0 == ans) {
                return Err(ApiError::bad(format!("No answer “{ans}”. Answers: {}", p.answers.iter().map(|x| x.name.clone()).chain(plugin::GENERIC.iter().map(|g| g.0.to_string())).collect::<Vec<_>>().join(", "))));
            }
            let until = a.ms("for")?.map(|m| util::now_ms() + m as i64);
            let count = if until.is_some() { a.u64("count").map(|c| c as i64) } else { Some(a.u64("count").unwrap_or(1) as i64) };
            store::force(&ctx, &svc, &ans, &a.str("route").unwrap_or_default(), count, until, &by(&a)).await?;
            Ok(json!({"service": svc, "forced": store::forced(&ctx, &svc).await?}))
        })
        .req("service", "plugin").req("answer", "a named answer, a generic one, or default to stop").param("count", "next N calls (default 1)").param("for", "a time, e.g. 10m").param("route", "only this “METHOD /path”")
        .route("POST", "/apis/{service}/force"),
        Tool::new("billing", "Billable calls and the counter value per service (price per call is only a counter); repeated inputs point at a retry loop.", |ctx, a| async move {
            let s = since(&a, "1d")?;
            let counts = store::counts(&ctx, s).await?;
            let mut out = vec![];
            for p in plugin::all(&ctx).into_iter().filter(|p| a.str("service").map(|x| x == p.service.name).unwrap_or(true)) {
                let (calls, billable) = counts.get(&p.service.name).cloned().unwrap_or((0, 0));
                out.push(json!({"service": p.service.name, "calls": calls, "billable": billable, "perCall": p.service.billing.per_call, "total": billable as f64 * p.service.billing.per_call,
                    "warnAbove": p.service.billing.warn_above, "repeated": store::repeated(&ctx, &p.service.name, s).await?}));
            }
            Ok(json!({"since": s, "services": out}))
        })
        .param("service", "plugin").param("since", "1d (default)")
        .route("GET", "/apis/billing"),
    ]
}

// ---------------------------------------------------------------- A2 unmatched

fn unmatched_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_unmatched", "Requests that reached Stuntman but no route took (answered 404 “no route”), grouped by path | service | caller | host (or one by one with group=none), with the closest route.", |ctx, a| async move {
            let s = since(&a, "1h")?;
            let all = store::unmatched(&ctx, s, a.bool("ignored").unwrap_or(false)).await?;
            let group = a.str("group").unwrap_or("path".into());
            // One by one: every call with its headers, body and the closest route, newest first.
            if group == "none" {
                return Ok(json!({"group": group, "count": all.len(), "calls": all}));
            }
            let mut groups: Vec<(String, Vec<Value>)> = vec![];
            for u in all {
                let k = match group.as_str() {
                    "service" => if u["service"].as_str().unwrap_or("").is_empty() { "no plugin for the host".to_string() } else { u["service"].as_str().unwrap().to_string() },
                    "caller" => u["caller"].as_str().unwrap_or("").to_string(),
                    "host" => u["host"].as_str().unwrap_or("").to_string(),
                    _ => format!("{} {}", u["method"].as_str().unwrap_or(""), u["path"].as_str().unwrap_or("")),
                };
                match groups.iter_mut().find(|g| g.0 == k) {
                    Some(g) => g.1.push(u),
                    None => groups.push((k, vec![u])),
                }
            }
            Ok(json!({"group": group, "groups": groups.into_iter().map(|(k, v)| json!({"key": k, "count": v.len(), "first": v.last().map(|x| x["at"].clone()), "latest": v[0].clone(),
                "callers": v.iter().map(|x| x["caller"].as_str().unwrap_or("").to_string()).collect::<std::collections::BTreeSet<_>>()})).collect::<Vec<_>>()}))
        })
        .param("group", "path (default) | service | caller | host | none (each call, one by one: request.path as received with the service prefix, request.servicePath after it, query string, headers and body)").param("since", "1h (default)").param("ignored", "true: show ignored too")
        .route("GET", "/apis/unmatched"),
        Tool::new("route_from_request", "A draft route prefilled from an unmatched request (method, path, body fields; the closest route's steps copied). Returns its YAML; set_route saves it.", |ctx, a| async move {
            let u = store::unmatched_one(&ctx, &a.need("id")?).await?;
            let plugins = plugin::all(&ctx);
            let r = draft_route_from(&u, &plugins);
            let svc = u["closest"]["service"].as_str().or(u["service"].as_str()).unwrap_or("").to_string();
            Ok(json!({"service": svc, "route": r, "yaml": serde_yaml::to_string(&vec![r.clone()]).unwrap_or_default()}))
        })
        .req("id", "unmatched request id")
        .route("POST", "/apis/unmatched/{id}/route"),
        Tool::new("ignore_unmatched", "Hides an unmatched request (and the same method and path); it is still answered 404.", |ctx, a| async move {
            Ok(json!({"ignored": store::ignore(&ctx, &a.need("id")?).await?}))
        })
        .req("id", "unmatched request id")
        .route("POST", "/apis/unmatched/{id}/ignore"),
        Tool::new("real_provider_calls", "Hosts the app called directly, from telemetry: calls that never came through Stuntman.", |ctx, a| async move {
            let s = since(&a, "1h")? * 1_000_000;
            let db = stuntman_telemetry::db(&ctx);
            let rows = db
                .call(move |c| {
                    let mut q = c.prepare("SELECT host, count(*), group_concat(DISTINCT service), max(start_ns) FROM tel_spans WHERE kind=3 AND dub=0 AND host!='' AND start_ns>=?1 GROUP BY host ORDER BY count(*) DESC LIMIT 50")?;
                    let v = q.query_map([s], |r| Ok(json!({"host": r.get::<_, String>(0)?, "calls": r.get::<_, i64>(1)?, "services": r.get::<_, String>(2)?, "last": r.get::<_, i64>(3)? / 1_000_000})))?.collect::<Result<Vec<_>, _>>();
                    v
                })
                .await?;
            let local = ["localhost", "127.0.0.1", "stuntman", "duble"];
            Ok(json!({"hosts": rows.into_iter().filter(|r| !local.iter().any(|l| r["host"].as_str().unwrap_or("").contains(l))).collect::<Vec<_>>()}))
        })
        .param("since", "1h (default)")
        .route("GET", "/apis/real-provider-calls"),
    ]
}

// ---------------------------------------------------------------- A3 plugin and routes

fn plugin_view(ctx: &Ctx, name: &str) -> Result<Value, ApiError> {
    let p = plugin::get(ctx, name)?;
    let t = plugin::text(ctx, name);
    Ok(json!({"service": name, "plugin": p, "yaml": t, "version": plugin::version(&t), "file": plugin::rel(name), "errors": plugin::validate(&p, &t)}))
}

fn plugin_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_plugins", "The catalog: plugins you can install (origin, region, badges, installed here, update available) and its collections. Filters: query, region (Global | Brazil), category (Browser pages), collection.", |ctx, a| async move {
            Ok(crate::catalog::list(&ctx, &a.str("query").unwrap_or_default(), &a.str("region").unwrap_or_default(), &a.str("category").unwrap_or_default(), &a.str("collection").unwrap_or_default()))
        })
        .param("query", "words in the name, title, description or host").param("region", "Any | Global | Brazil").param("category", "HTTP APIs | Browser pages").param("collection", "a collection name")
        .route("GET", "/plugins"),
        Tool::new("install_plugin", "Installs a plugin from the catalog (or an empty one that records: plugin=empty, service=<name>) into this environment.", |ctx, a| async move {
            let name = a.need("plugin")?;
            let (t, svc) = if name == "empty" {
                let svc = a.need("service")?;
                plugin::valid_name(&svc)?;
                let routes = ["GET", "POST", "PUT", "PATCH", "DELETE"].iter().map(|m| Route { route: format!("{m} /{{*}}"), decide: vec![Decide { when: None, answer: "ok".into(), ..Default::default() }], ..Default::default() }).collect();
                let p = Plugin { service: plugin::Service { name: svc.clone(), title: a.str("title").unwrap_or(svc.clone()), origin: "yours".into(), base_url: a.str("base_url").unwrap_or_default(), ..Default::default() },
                    routes,
                    answers: vec![Answer { name: "ok".into(), status: 200, body: "{\"ok\": true}".into(), sentence: "{{ caller }} called {{ path.rest | default('') }}".into(), ..Default::default() }], ..Default::default() };
                (serde_yaml::to_string(&p).unwrap_or_default(), svc)
            } else {
                let path = plugin::catalog_dir().join(format!("{name}.yaml"));
                let t = std::fs::read_to_string(&path).map_err(|_| ApiError::not_found(format!("No plugin “{name}” in the catalog")))?;
                match a.str("service").filter(|s| !s.is_empty() && *s != name) {
                    // Another instance of the same plugin, under its own name: reached by the
                    // /<service>/ prefix; the real host stays with the first one.
                    Some(svc) => {
                        plugin::valid_name(&svc)?;
                        let mut y: serde_yaml::Value = serde_yaml::from_str(&t).map_err(|e| ApiError::internal(e.to_string()))?;
                        let title = y["service"]["title"].as_str().unwrap_or(&name).to_string();
                        y["service"]["name"] = svc.clone().into();
                        y["service"]["title"] = format!("{title} ({svc})").into();
                        if let Some(m) = y["service"].as_mapping_mut() { m.remove("real_host"); }
                        (serde_yaml::to_string(&y).unwrap_or_default(), svc)
                    }
                    None => (t, name),
                }
            };
            if plugin::file(&ctx, &svc).exists() {
                return Err(ApiError::new(409, format!("{svc} is already installed")));
            }
            plugin::save_text(&ctx, &svc, &t)?;
            Ok(plugin_view(&ctx, &svc)?)
        })
        .req("plugin", "a catalog name, or empty").param("service", "for empty: the service name; for a catalog plugin: a second instance under this name").param("base_url", "for empty: the provider's real base URL")
        .route("POST", "/plugins"),
        Tool::new("remove_api_service", "Removes a plugin from this environment (calls stay in the history).", |ctx, a| async move {
            let s = service_of(&a)?;
            plugin::get(&ctx, &s)?;
            std::fs::remove_file(plugin::file(&ctx, &s)).map_err(|e| ApiError::internal(e.to_string()))?;
            plugin::forget();
            for d in [crate::soap::folder(&ctx, &s), crate::openapi::folder(&ctx, &s), crate::record::folder(&ctx, &s), crate::record::files_dir(&ctx, &s)] {
                let _ = std::fs::remove_dir_all(d);
            }
            store::wipe_service(&ctx, &s).await?;
            ctx.live.send("apis.plugin", &s, "deleted");
            Ok(json!({"removed": s}))
        })
        .req("service", "plugin")
        .route("DELETE", "/plugins/{service}"),
        Tool::new("get_plugin", "The plugin YAML: service block, routes, answers, data; and its version (send it back to save over it).", |ctx, a| async move { plugin_view(&ctx, &service_of(&a)?) })
            .req("service", "plugin")
            .route("GET", "/plugins/{service}"),
        Tool::new("put_plugin", "Replaces the whole plugin YAML; validated before saving (409 if it changed since the version you read).", |ctx, a| async move {
            let s = service_of(&a)?;
            let cur = plugin::text(&ctx, &s);
            if let Some(v) = a.str("version") {
                if !cur.is_empty() && v != plugin::version(&cur) {
                    return Err(ApiError::new(409, format!("{} changed elsewhere", plugin::rel(&s))).with(json!({"yaml": cur, "version": plugin::version(&cur)})));
                }
            }
            plugin::save_text(&ctx, &s, &a.need("yaml")?)?;
            plugin_view(&ctx, &s)
        })
        .req("service", "plugin").req("yaml", "the whole file").param("version", "the version you read")
        .route("PUT", "/plugins/{service}"),
        Tool::new("set_route", "Creates or replaces a route (method, path, steps: receive, let, keep, wait, decide, respond, send, fail, billable); validated before saving.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let cur = plugin::text(&ctx, &s);
            if let Some(v) = a.str("version") {
                if v != plugin::version(&cur) {
                    return Err(ApiError::new(409, format!("{} changed elsewhere", plugin::rel(&s))).with(json!({"yaml": cur, "version": plugin::version(&cur)})));
                }
            }
            let method = a.need("method")?.to_uppercase();
            let path = a.need("path")?;
            let mut r: Route = match a.get("steps").or(a.get("route_steps")) {
                Some(v) => serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("steps: {e}")))?,
                None => Route::default(),
            };
            r.route = format!("{method} {path}");
            let old = a.str("old_route");
            let key = old.clone().unwrap_or(r.route.clone());
            match p.routes.iter().position(|x| x.route == key) {
                Some(i) => p.routes[i] = r.clone(),
                None => p.routes.push(r.clone()),
            }
            let v = plugin::save(&ctx, &p)?;
            Ok(json!({"route": r, "version": v}))
        })
        .req("service", "plugin").req("method", "GET, POST…").req("path", "/path/{param}").param("steps", "{receive, let, keep, wait, decide, respond, send, fail, billable, schema}").param("old_route", "“METHOD /path” to replace (rename)").param("version", "the version you read")
        .route("PUT", "/plugins/{service}/routes"),
        Tool::new("delete_route", "Deletes a route.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let key = format!("{} {}", a.need("method")?.to_uppercase(), a.need("path")?);
            let n = p.routes.len();
            p.routes.retain(|r| r.route != key);
            if p.routes.len() == n {
                return Err(ApiError::not_found(format!("No route {key}")));
            }
            plugin::save(&ctx, &p)?;
            Ok(json!({"deleted": key}))
        })
        .req("service", "plugin").req("method", "method").req("path", "path")
        .route("DELETE", "/plugins/{service}/routes"),
        Tool::new("try_route", "Runs a route's steps on a sample request; nothing is kept, counted, billed or sent. steps= tries an unsaved version of the route.", |ctx, a| async move {
            let s = service_of(&a)?;
            let p = plugin::get(&ctx, &s)?;
            let method = a.need("method")?.to_uppercase();
            let path = a.need("path")?;
            let route: Route = match a.get("steps") {
                Some(v) => {
                    let mut r: Route = serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("steps: {e}")))?;
                    r.route = format!("{method} {path}");
                    r
                }
                None => p.routes.iter().find(|r| r.method_path() == (method.clone(), path.clone())).cloned().ok_or_else(|| ApiError::not_found(format!("No route {method} {path}")))?,
            };
            let rq = a.get("request").cloned().unwrap_or(json!({}));
            let sample_path = rq["path"].as_str().unwrap_or(&path).to_string();
            let params = engine::match_path(&route.method_path().1, &sample_path).unwrap_or_default();
            let headers: std::collections::BTreeMap<String, String> = rq["headers"].as_object().map(|o| o.iter().map(|(k, v)| (k.to_lowercase(), v.as_str().unwrap_or("").to_string())).collect()).unwrap_or_default();
            let mut req = engine::Req { method: method.clone(), path: sample_path, headers, body: rq["body"].clone(), caller: "try it".into(), ..Default::default() };
            if let Some(q) = rq["query"].as_object() {
                req.query = q.clone();
            }
            let o = engine::run(&ctx, &p, &route, params, &req, true).await;
            let body = String::from_utf8_lossy(&o.body).to_string();
            Ok(json!({"status": o.status, "answer": o.answer, "sentence": o.sentence, "steps": o.steps, "latencyMs": o.latency_ms,
                "headers": o.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(), "body": serde_json::from_str::<Value>(&body).unwrap_or(json!(body)), "network": o.network,
                "note": "Nothing is stored and nothing is billed."}))
        })
        .req("service", "plugin").req("method", "method").req("path", "the route's path").param("request", "{path?, headers, query, body}").param("steps", "an unsaved version of the route")
        .route("POST", "/plugins/{service}/try"),
        Tool::new("validate_plugin", "Schema and expression errors with line numbers; answers checked against the routes' schema. yaml= checks text before saving.", |ctx, a| async move {
            let s = service_of(&a)?;
            let t = a.str("yaml").unwrap_or_else(|| plugin::text(&ctx, &s));
            let p = match plugin::parse(&t) {
                Ok(p) => p,
                Err(e) => return Ok(json!({"ok": false, "errors": [{"error": e, "line": e.strip_prefix("line ").and_then(|x| x.split(':').next()).and_then(|x| x.parse::<u64>().ok())}]})),
            };
            let mut errs = plugin::validate(&p, &t);
            for r in &p.routes {
                for d in &r.decide {
                    if let Some(a2) = p.answers.iter().find(|x| x.name == d.answer) {
                        if let Some(e) = engine::check_answer(r, a2, &p) {
                            errs.push(json!({"error": format!("answer {}: {e}", a2.name), "line": t.lines().position(|l| l.contains(&format!("name: {}", a2.name))).map(|i| i + 1)}));
                        }
                    }
                }
            }
            Ok(json!({"ok": errs.is_empty(), "errors": errs}))
        })
        .req("service", "plugin").param("yaml", "text to check instead of the saved file")
        .route("POST", "/plugins/{service}/validate"),
    ]
}

// ---------------------------------------------------------------- A4 answers and data

fn answers_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_answers", "Named answers: status, body, and which route step uses each (an answer used by none exists only to be forced).", |ctx, a| async move {
            let s = service_of(&a)?;
            let p = plugin::get(&ctx, &s)?;
            let out: Vec<Value> = p.answers.iter().map(|x| {
                let mut used = vec![];
                for r in p.routes.iter().chain(p.operations.iter()) {
                    let label = if !r.route.is_empty() { r.route.clone() } else { r.operation.clone() };
                    for (i, d) in r.decide.iter().enumerate() {
                        if d.answer == x.name {
                            used.push(json!({"route": label, "step": "decide", "rule": i, "when": d.when}));
                        }
                    }
                    if let Some(w) = &r.wait {
                        if w.answer == x.name || w.then == x.name {
                            used.push(json!({"route": label, "step": "wait"}));
                        }
                    }
                    if r.receive.missing == x.name {
                        used.push(json!({"route": label, "step": "receive"}));
                    }
                }
                let problem = p.routes.iter().filter(|r| used.iter().any(|u| u["route"] == json!(r.route))).find_map(|r| engine::check_answer(r, x, &p));
                json!({"answer": x, "usedBy": used, "problem": problem})
            }).collect();
            Ok(json!({"service": s, "answers": out, "generic": plugin::GENERIC.iter().map(|g| json!({"name": g.0, "label": g.1})).collect::<Vec<_>>()}))
        })
        .req("service", "plugin")
        .route("GET", "/plugins/{service}/answers"),
        Tool::new("set_answer", "Creates or replaces a named answer (status, body template, headers, sentence); checked against the schema unless raw=true (malformed on purpose); fault={code, reason} makes it a SOAP fault; stream=[{after, event, data}] sends it in parts (stream_format sse | ndjson | chunks); template=handlebars renders WireMock templates; latency waits before answering.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let name = a.need("name")?;
            let old = a.str("old_name").unwrap_or(name.clone());
            let mut ans = p.answers.iter().find(|x| x.name == old).cloned().unwrap_or(Answer { name: name.clone(), status: 200, ..Default::default() });
            ans.name = name.clone();
            if let Some(v) = a.u64("status") {
                ans.status = v as u16;
            }
            if let Some(b) = a.get("body") {
                ans.body = match b { Value::String(s) => s.clone(), o => serde_json::to_string_pretty(o).unwrap_or_default() };
            }
            if let Some(h) = a.get("headers").and_then(|h| h.as_object()) {
                ans.headers = h.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect();
            }
            if let Some(r) = a.bool("raw") {
                ans.raw = r;
            }
            if let Some(t) = a.str("sentence") {
                ans.sentence = t;
            }
            if let Some(t) = a.str("note") {
                ans.note = t;
            }
            if let Some(v) = a.get("stream") {
                ans.stream = if v.is_null() { vec![] } else { serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("stream: [{{after, event, data}}]: {e}")))? };
            }
            if let Some(t) = a.str("stream_format") {
                ans.stream_format = if t == "sse" { String::new() } else { t };
            }
            if let Some(t) = a.get("template") {
                ans.template = t.as_str().unwrap_or("").to_string();
            }
            if let Some(t) = a.str("encode") {
                ans.encode = t;
            }
            if let Some(t) = a.get("latency") {
                ans.latency = t.as_str().unwrap_or("").to_string();
            }
            if let Some(v) = a.get("rpc_error") {
                ans.rpc_error = v.as_i64();
            }
            match a.get("fault") {
                Some(Value::Null) | Some(Value::Bool(false)) => ans.fault = None,
                Some(f) => ans.fault = Some(serde_json::from_value(f.clone()).map_err(|e| ApiError::bad(format!("fault: {{code, reason}}: {e}")))?),
                None => {}
            }
            for r in p.routes.iter().filter(|r| r.decide.iter().any(|d| d.answer == old)) {
                if let Some(e) = engine::check_answer(r, &ans, &p) {
                    return Err(ApiError::bad(e).with(json!({"route": r.route})));
                }
            }
            if old != name {
                for r in p.routes.iter_mut().chain(p.operations.iter_mut()) {
                    for d in r.decide.iter_mut().filter(|d| d.answer == old) {
                        d.answer = name.clone();
                    }
                }
            }
            match p.answers.iter().position(|x| x.name == old) {
                Some(i) => p.answers[i] = ans.clone(),
                None => p.answers.push(ans.clone()),
            }
            plugin::save(&ctx, &p)?;
            Ok(json!({"answer": ans}))
        })
        .req("service", "plugin").req("name", "answer name").param("status", "HTTP status").param("body", "template (text) or JSON").param("headers", "{name: value}").param("raw", "true: sent as typed").param("encode", "gzip | deflate | zip: compress the rendered body on the way out (raw bytes, no Content-Encoding)").param("stream", "[{after, event, data}]").param("stream_format", "sse | ndjson | chunks").param("template", "handlebars").param("latency", "800ms").param("rpc_error", "MCP: a JSON-RPC error code")
        .param("sentence", "the sentence on Calls (template)").param("note", "what it means").param("old_name", "rename")
        .param("fault", "SOAP: {code: Sender | Receiver, reason} makes it a fault (the body is its detail); null makes it a response again")
        .route("PUT", "/plugins/{service}/answers/{name}"),
        Tool::new("delete_answer", "Deletes a named answer that no route uses.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let n = a.need("name")?;
            if p.routes.iter().chain(p.operations.iter()).any(|r| r.decide.iter().any(|d| d.answer == n)) {
                return Err(ApiError::new(409, format!("“{n}” is used by a route; change the route first")));
            }
            p.answers.retain(|x| x.name != n);
            plugin::save(&ctx, &p)?;
            Ok(json!({"deleted": n}))
        })
        .req("service", "plugin").req("name", "answer")
        .route("DELETE", "/plugins/{service}/answers/{name}"),
        Tool::new("get_data", "A data table the answers read (irregular_cpfs, people, selfies…), or every table.", |ctx, a| async move {
            let p = plugin::get(&ctx, &service_of(&a)?)?;
            Ok(match a.str("table") {
                Some(t) => json!({"table": t, "rows": p.data.get(&t).cloned().unwrap_or(json!([]))}),
                None => json!({"tables": p.data}),
            })
        })
        .req("service", "plugin").param("table", "table name")
        .route("GET", "/plugins/{service}/data").route("GET", "/plugins/{service}/data/{table}"),
        Tool::new("set_data", "Replaces (or with add=true appends to) a data table as the plugin declares it.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let t = a.need("table")?;
            let rows = match a.get("rows") { Some(Value::Array(r)) => r.clone(), Some(Value::Object(o)) => vec![Value::Object(o.clone())], _ => return Err(ApiError::bad("rows: a list of objects")) };
            let entry = p.data.entry(t.clone()).or_insert(json!([]));
            if a.bool("add").unwrap_or(false) {
                if let Some(v) = entry.as_array_mut() {
                    v.extend(rows);
                }
            } else {
                *entry = Value::Array(rows);
            }
            plugin::save(&ctx, &p)?;
            Ok(json!({"table": t, "rows": p.data[&t]}))
        })
        .req("service", "plugin").req("table", "table").req("rows", "list of objects").param("add", "true: append")
        .route("PUT", "/plugins/{service}/data/{table}"),
        Tool::new("synthetic_cpf", "A CPF that passes the check digits and is not a real person's (for data tables and tests).", |_ctx, _a| async move {
            let mut d: Vec<u32> = (0..9).map(|_| rand::random::<u32>() % 10).collect();
            d[0] = 9;
            for n in [9usize, 10] {
                let s: u32 = (0..n).map(|i| d[i] * (n as u32 + 1 - i as u32)).sum();
                let r = (s * 10) % 11;
                d.push(if r == 10 { 0 } else { r });
            }
            let c: String = d.iter().map(|x| x.to_string()).collect();
            Ok(json!({"cpf": c, "formatted": format!("{}.{}.{}-{}", &c[..3], &c[3..6], &c[6..9], &c[9..])}))
        })
        .route("GET", "/apis/synthetic-cpf"),
    ]
}

// ---------------------------------------------------------------- A5 service block

fn service_tools() -> Vec<Tool> {
    vec![
        Tool::new("get_api_service", "A service block: base URL and real host, auth (OAuth2 client credentials, Basic), billing, limits; plus Stuntman's URLs.", |ctx, a| async move {
            let s = service_of(&a)?;
            let p = plugin::get(&ctx, &s)?;
            let t = plugin::text(&ctx, &s);
            let mut v = service_view(&ctx, &p).await;
            v["service"] = json!(p.service);
            v["yaml"] = json!(t);
            v["version"] = json!(plugin::version(&t));
            v["file"] = json!(plugin::rel(&s));
            v["tokenRoute"] = json!(p.routes.iter().find(|r| r.token).map(|r| r.route.clone()));
            Ok(v)
        })
        .req("service", "plugin")
        .route("GET", "/plugins/{service}/service"),
        Tool::new("set_service", "Changes the service block: any_client, token_ttl, billing (enabled, per_call, warn_above), latency, rate, fail_share, base_url, real_host, title; webhook signing (webhook_signing, webhook_header, webhook_secret, webhook_tolerance, webhook_provoke), idempotency, xml_signature_* and client_certificate_icp.", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let cur = plugin::text(&ctx, &s);
            if let Some(v) = a.str("version") {
                if v != plugin::version(&cur) {
                    return Err(ApiError::new(409, format!("{} changed elsewhere", plugin::rel(&s))).with(json!({"yaml": cur, "version": plugin::version(&cur)})));
                }
            }
            let sv = &mut p.service;
            if let Some(x) = a.bool("any_client") {
                sv.auth.oauth2_client_credentials.get_or_insert_with(Default::default).any_client = x;
            }
            if let Some(x) = a.bool("client_certificate") {
                sv.auth.client_certificate = if x { Some(plugin::ClientCertificate { required: true, icp: sv.auth.client_certificate.as_ref().map(|c| c.icp).unwrap_or(false) }) } else { None };
            }
            if let Some(x) = a.bool("client_certificate_icp") {
                sv.auth.client_certificate.get_or_insert(plugin::ClientCertificate { required: true, icp: false }).icp = x;
            }
            if let Some(x) = a.str("session_ttl") {
                util::parse_ms(&x).ok_or_else(|| ApiError::bad("session_ttl: 30m, 5m, 1m"))?;
                sv.auth.session.get_or_insert_with(Default::default).ttl = x;
            }
            if let Some(x) = a.str("token_ttl") {
                util::parse_ms(&x).ok_or_else(|| ApiError::bad("token_ttl: 60m, 10m, 1m"))?;
                sv.auth.oauth2_client_credentials.get_or_insert_with(Default::default).token_ttl = x;
            }
            if let Some(x) = a.bool("billing") {
                sv.billing.enabled = x;
            }
            if let Some(x) = a.get("per_call").and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.replace(',', ".").parse().ok()))) {
                sv.billing.per_call = x;
            }
            if let Some(x) = a.str("warn_above") {
                if !x.is_empty() && plugin::per(&x).is_none() {
                    return Err(ApiError::bad("warn_above: like 100/day"));
                }
                sv.billing.warn_above = x;
            }
            if let Some(x) = a.str("latency") {
                sv.limits.latency = if x == "none" { String::new() } else { x };
            }
            if let Some(x) = a.str("rate") {
                if !x.is_empty() && x != "none" && plugin::per(&x).is_none() {
                    return Err(ApiError::bad("rate: like 10/s"));
                }
                sv.limits.rate = if x == "none" { String::new() } else { x };
            }
            if let Some(v) = a.get("fail_share") {
                let f = v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim_end_matches('%').trim().parse::<f64>().ok().map(|n| if s.contains('%') { n / 100.0 } else { n }))).ok_or_else(|| ApiError::bad("fail_share: 0 to 1, or 50%"))?;
                sv.limits.fail_share = if f > 1.0 { f / 100.0 } else { f };
            }
            if let Some(x) = a.str("webhook_signing") {
                if x == "none" {
                    sv.webhooks = None;
                } else {
                    if !plugin::SIGNS.contains(&x.as_str()) {
                        return Err(ApiError::bad(format!("webhook_signing: {} | none", plugin::SIGNS.join(" | "))));
                    }
                    sv.webhooks.get_or_insert_with(|| plugin::Webhooks { tolerance: 300, ..Default::default() }).sign = x;
                }
            }
            if let Some(w) = sv.webhooks.as_mut() {
                if let Some(x) = a.str("webhook_secret") {
                    w.secret = x;
                }
                if let Some(x) = a.str("webhook_header") {
                    w.header = x;
                }
                if let Some(x) = a.get("webhook_tolerance").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| util::parse_ms(s).map(|m| m / 1000)))) {
                    w.tolerance = x as u32;
                }
                if let Some(v) = a.get("webhook_provoke") {
                    let list: Vec<String> = match v { Value::Array(x) => x.iter().filter_map(|y| y.as_str().map(String::from)).collect(), Value::String(x) => x.split(',').map(|y| y.trim().to_string()).filter(|y| !y.is_empty() && y != "none").collect(), _ => vec![] };
                    if let Some(bad) = list.iter().find(|x| !plugin::SIGN_PROVOKE.contains(&x.as_str())) {
                        return Err(ApiError::bad(format!("webhook_provoke “{bad}”: {}", plugin::SIGN_PROVOKE.join(" | "))));
                    }
                    w.provoke = list;
                }
            }
            if a.get("xml_signature_check").is_some() || a.get("xml_signature_sign").is_some() || a.get("xml_signature_invalid").is_some() || a.get("xml_signature_sha256").is_some() {
                let sp = p.soap.as_mut().ok_or_else(|| ApiError::bad("XML signatures are for SOAP services"))?;
                let sg = sp.signature.get_or_insert_with(Default::default);
                if let Some(x) = a.bool("xml_signature_check") { sg.check = x; }
                if let Some(x) = a.get("xml_signature_sign") { sg.sign = x.as_str().unwrap_or("").to_string(); }
                if let Some(x) = a.get("xml_signature_invalid") { sg.invalid = x.as_str().unwrap_or("").to_string(); }
                if let Some(x) = a.bool("xml_signature_sha256") { sg.sha256 = x; }
                if !sg.check && sg.sign.is_empty() { sp.signature = None; }
            }
            let sv = &mut p.service;
            if let Some(v) = a.get("idempotency") {
                sv.idempotency = match v {
                    Value::Bool(false) => None,
                    Value::String(h) if h == "none" || h.is_empty() => None,
                    Value::String(h) => Some(plugin::Idempotency { header: h.clone(), ttl: "24h".into() }),
                    _ => Some(plugin::Idempotency { header: "Idempotency-Key".into(), ttl: "24h".into() }),
                };
            }
            for (k, f) in [("base_url", &mut sv.base_url), ("title", &mut sv.title), ("description", &mut sv.description)] {
                if let Some(x) = a.str(k) {
                    *f = x;
                }
            }
            if a.get("real_host").is_some() {
                sv.real_host = a.list("real_host").into();
            }
            let v = plugin::save(&ctx, &p)?;
            Ok(json!({"service": p.service, "version": v}))
        })
        .req("service", "plugin").param("any_client", "bool").param("token_ttl", "60m").param("client_certificate", "bool: require the app's certificate (mTLS) on HTTPS").param("client_certificate_icp", "bool: the certificate must carry a CNPJ (ICP-Brasil e-CNPJ)").param("session_ttl", "30m: session-cookie portals").param("billing", "bool: count billable calls").param("per_call", "price").param("warn_above", "100/day")
        .param("latency", "300ms..600ms | none").param("rate", "10/s | none").param("fail_share", "0 to 1, or 50%").param("base_url", "real base URL").param("real_host", "real host name, or a list (several names, comma-separated)").param("title", "display name").param("version", "the version you read")
        .param("webhook_signing", "hex | hex_ts | stripe | svix | github | cloudevents | twilio | slack | shopify | hubspot | none").param("webhook_secret", "secret://name").param("webhook_header", "the signature header").param("webhook_tolerance", "seconds, 300")
        .param("webhook_provoke", "wrong_signature, old_timestamp, no_signature, twice, out_of_order | none").param("idempotency", "true | false | the header name (Idempotency-Key)")
        .param("xml_signature_check", "SOAP: bool, check the request's XML signature").param("xml_signature_sign", "SOAP: the answer element to sign (infProt), empty: none").param("xml_signature_invalid", "SOAP: the answer for a bad signature").param("xml_signature_sha256", "SOAP: bool, RSA-SHA256 instead of RSA-SHA1")
        .route("PUT", "/plugins/{service}/service"),
        Tool::new("clock", "The environment's clock (real time plus what advance_clock moved) and the webhooks waiting for their time.", |ctx, _a| async move {
            let now = stuntman_core::clock::now(&ctx);
            Ok(json!({"now": crate::expr::iso(now, None), "nowMs": now, "offsetMs": stuntman_core::clock::offset(&ctx), "waiting": store::scheduled(&ctx).await?}))
        })
        .route("GET", "/clock"),
        Tool::new("advance_clock", "Moves the environment's clock forward (1h, 1d, 30d): sends that wait (delay) and now() in templates follow it. reset=true goes back to real time.", |ctx, a| async move {
            if a.bool("reset") == Some(true) {
                stuntman_core::clock::reset(&ctx);
            } else {
                let by = a.need("by")?;
                let ms = util::parse_ms(&by).ok_or_else(|| ApiError::bad("by: 30m, 1h, 1d, 30d"))?;
                stuntman_core::clock::advance(&ctx, ms as i64);
            }
            crate::engine::fire_due(&ctx).await;
            ctx.live.send("clock", "", "updated");
            let now = stuntman_core::clock::now(&ctx);
            Ok(json!({"now": crate::expr::iso(now, None), "offsetMs": stuntman_core::clock::offset(&ctx), "waiting": store::scheduled(&ctx).await?}))
        })
        .param("by", "30m | 1h | 1d | 30d").param("reset", "bool: back to real time")
        .route("POST", "/clock/advance"),
        Tool::new("set_mcp_tool", "Adds or changes a tool of a service's MCP server (the service gets an mcp: block on /mcp if it has none). decide: [{when, answer}] like a route; answers are the tool results (status >= 400 gives isError).", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let name = a.need("name")?;
            p.mcp.get_or_insert_with(|| plugin::Mcp { path: "/mcp".into(), version: "1.0.0".into(), ..Default::default() });
            let decide: Vec<plugin::Decide> = match a.get("decide") { Some(v) => serde_json::from_value(v.clone()).map_err(|e| ApiError::bad(format!("decide: {e}")))?, None => vec![] };
            let i = p.routes.iter().position(|r| r.tool == name);
            let r = match i { Some(i) => &mut p.routes[i], None => { p.routes.push(plugin::Route { tool: name.clone(), ..Default::default() }); p.routes.last_mut().unwrap() } };
            if let Some(d) = a.str("description") {
                r.description = d;
            }
            if let Some(v) = a.get("input_schema") {
                r.input_schema = Some(v.clone());
            }
            if !decide.is_empty() {
                r.decide = decide;
            }
            let errs = plugin::validate(&p, &serde_yaml::to_string(&p).unwrap_or_default());
            if let Some(e) = errs.first() {
                return Err(ApiError::bad(e["error"].as_str().unwrap_or("invalid").to_string()));
            }
            let v = plugin::save(&ctx, &p)?;
            Ok(json!({"service": s, "tools": crate::mcp::tools(&p), "version": v}))
        })
        .req("service", "plugin").req("name", "tool name").param("description", "what it does").param("input_schema", "JSON Schema of the arguments").param("decide", "[{when, answer}]")
        .route("PUT", "/plugins/{service}/mcp-tools"),
        Tool::new("get_test_ca", "The Stuntman Test CA: download URLs (PEM, DER .crt, PKCS#12 truststore/PFX without a key), its file on disk, and how to trust it per runtime and OS (Linux, macOS, Windows; .NET, Node, Python, Java, Go, curl). Prefer the per-process options; ask the developer before adding it to an OS trust store.", |_ctx, _a| async move {
            let c = crate::ca::get();
            let base = format!("http://localhost:{}/api", std::env::var("STUNTMAN_PORT").unwrap_or("8025".into()));
            let pem = crate::ca::keys_dir().join("test-ca.pem");
            let der = crate::ca::der_of(&c.pem).first().map(|d| d.to_vec()).unwrap_or_default();
            // The subject name, for the undo commands (older data folders keep the CA they made).
            let cn = x509_parser::parse_x509_certificate(&der).ok().and_then(|(_, x)| x.subject().iter_common_name().next().and_then(|n| n.as_str().ok().map(String::from))).unwrap_or("Stuntman Test CA".into());
            Ok(json!({"pem": c.pem, "name": cn, "fingerprint": crate::tls::fingerprint(crate::ca::der_of(&c.pem).first().map(|d| d.as_ref()).unwrap_or(&[])), "httpsPort": crate::tls_port(),
                "urls": {"pem": format!("{base}/test-ca.pem"), "crt": format!("{base}/test-ca.crt"), "der": format!("{base}/test-ca.der"), "p12": format!("{base}/test-ca.p12"), "pfx": format!("{base}/test-ca.pfx")},
                "path": pem.to_string_lossy(), "pathNote": "the file on the machine Stuntman runs on; in Docker it is inside the container, so download it from a URL instead",
                "truststorePassword": crate::ca::TRUSTSTORE_PASSWORD,
                "rule": "Trust it for the app's test process only (env var or test config). Adding it to an OS trust store (keychain, Windows store, update-ca-certificates) changes the developer's machine: ask first, never do it silently, and never on a machine that talks to real providers.",
                "runtimes": {
                    "node": "NODE_EXTRA_CA_CERTS=stuntman-test-ca.pem (every OS)",
                    "python": "REQUESTS_CA_BUNDLE=stuntman-test-ca.pem for requests (and the Azure SDKs), SSL_CERT_FILE=stuntman-test-ca.pem for ssl/httpx/aiohttp; certifi users: the same two variables (every OS)",
                    "java": "-Djavax.net.ssl.trustStore=stuntman-test-ca.p12 -Djavax.net.ssl.trustStoreType=PKCS12 -Djavax.net.ssl.trustStorePassword=changeit (every OS; replaces the default truststore for that JVM)",
                    "go": "SSL_CERT_FILE=stuntman-test-ca.pem (Linux; on macOS and Windows Go uses the OS verifier: load the PEM into tls.Config.RootCAs in test config)",
                    "curl": "curl --cacert stuntman-test-ca.pem (or CURL_CA_BUNDLE=stuntman-test-ca.pem)",
                    "dotnetLinux": "SSL_CERT_FILE=stuntman-test-ca.pem (OpenSSL reads it), or the system store below",
                    "dotnetMacOsWindows": ".NET ignores SSL_CERT_FILE there. In test config: a SocketsHttpHandler whose SslOptions.RemoteCertificateValidationCallback builds an X509Chain with TrustMode = CustomRootTrust and the CA in CustomTrustStore; pass it to Azure clients as options.Transport = new HttpClientTransport(handler), and to ServiceBusClientOptions/EventHubConnectionOptions as CertificateValidationCallback. Or, after asking, the user store below.",
                    "macOsKeychain": format!("security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db stuntman-test-ca.crt (user keychain; the developer confirms with a password or Touch ID). Undo: security delete-certificate -c \"{cn}\" ~/Library/Keychains/login.keychain-db"),
                    "windowsStore": format!("certutil -user -addstore Root stuntman-test-ca.crt, or PowerShell Import-Certificate -FilePath stuntman-test-ca.crt -CertStoreLocation Cert:\\CurrentUser\\Root (current user; Windows shows a confirmation dialog). Undo: certutil -user -delstore Root \"{cn}\""),
                    "linuxSystem": "sudo cp stuntman-test-ca.pem /usr/local/share/ca-certificates/stuntman-test-ca.crt && sudo update-ca-certificates (Debian/Ubuntu; in a container image this is the usual way). Undo: sudo rm /usr/local/share/ca-certificates/stuntman-test-ca.crt && sudo update-ca-certificates --fresh"
                }}))
        })
        .route("GET", "/apis/test-ca"),
        Tool::new("issue_client_certificate", "A client certificate signed by the Stuntman Test CA, for an app calling a provider that requires mTLS. Returns certificate and key PEM.", |_ctx, a| async move {
            let name = a.need("name")?;
            let (cert, key) = crate::ca::get().client(&name).map_err(ApiError::internal)?;
            crate::tls::remember_client(&cert, &name);
            Ok(json!({"name": name, "certificate": cert, "key": key}))
        })
        .req("name", "the app, e.g. acme-api (shown on Calls)")
        .route("POST", "/apis/client-certificates"),
        Tool::new("issue_person_certificate", "(Re)issues a person's cloud certificate (BirdID, ViDaaS people) by CPF with a validity window, signed by the Stuntman Test CA with the person's own key: expired yesterday by default, or not yet valid. Every plugin answers it until clear_person_certificate. Returns the chain PEM; keys stay in Stuntman.", |ctx, a| async move {
            let cpf = a.need("cpf")?;
            let digits: String = cpf.chars().filter(|c| c.is_ascii_digit()).collect();
            let name = match a.str("name") {
                Some(n) => n,
                None => plugin::all(&ctx).iter().flat_map(|p| p.data.get("people").and_then(Value::as_array).cloned().unwrap_or_default())
                    .find(|r| crate::expr::text(&r["cpf"]).chars().filter(|c| c.is_ascii_digit()).collect::<String>() == digits)
                    .map(|r| crate::expr::text(&r["name"]))
                    .ok_or_else(|| ApiError::bad(format!("No plugin has a person with CPF {cpf}: pass name")))?,
            };
            let nb = when(&a.str("not_before").unwrap_or("-365d".into()))?;
            let na = when(&a.str("not_after").unwrap_or("-1d".into()))?;
            let mut out = crate::people::issue_window(&cpf, &name, nb, na).map_err(ApiError::bad)?;
            out["notBefore"] = json!(iso_secs(nb));
            out["notAfter"] = json!(iso_secs(na));
            Ok(out)
        })
        .req("cpf", "123.456.789-09").param("name", "default: the person with this CPF in a plugin's people")
        .param("not_before", "ISO date/time (2026-01-01, 2026-01-01T12:00:00Z) or relative to now (-365d, +2d); default -365d")
        .param("not_after", "the same; default -1d (expired yesterday)")
        .route("POST", "/apis/people/{cpf}/certificate"),
        Tool::new("clear_person_certificate", "The person's certificate is again the plugin's (valid or expired as its people table says).", |_ctx, a| async move {
            Ok(json!({"cleared": crate::people::clear_window(&a.need("cpf")?).map_err(ApiError::bad)?}))
        })
        .req("cpf", "123.456.789-09")
        .route("DELETE", "/apis/people/{cpf}/certificate"),
        Tool::new("list_forced", "Who forced what: every forced answer and its stop, newest first.", |ctx, a| async move {
            Ok(json!({"forced": store::force_log(&ctx, a.str("service"), a.u64("last").unwrap_or(50) as i64).await?}))
        })
        .param("service", "plugin").param("last", "default 50")
        .route("GET", "/apis/forced"),
        Tool::new("expire_tokens", "Every token and session cookie issued by this service becomes invalid: the app's next call gets 401 and must ask (or log in) again.", |ctx, a| async move {
            let s = service_of(&a)?;
            Ok(json!({"expired": store::expire_tokens(&ctx, &s).await?}))
        })
        .req("service", "plugin")
        .route("POST", "/plugins/{service}/tokens/expire"),
    ]
}

// ---------------------------------------------------------------- K1–K3 catalog

fn catalog_tools() -> Vec<Tool> {
    vec![
        Tool::new("get_catalog_plugin", "One catalog plugin before installing it: routes, the pages it draws, its synthetic people, situations you can provoke, permissions and files.", |_ctx, a| async move {
            let name = a.need("plugin")?;
            let t = crate::catalog::text(&name)?;
            let p = plugin::parse(&t).map_err(ApiError::bad)?;
            let mut files = vec![json!({"name": format!("{name}.yaml"), "what": format!("{} lines", t.lines().count())})];
            if let Some(n) = p.data.get("people").and_then(|x| x.as_array()).map(|a| a.len()) {
                files.push(json!({"name": "data.people", "what": format!("{n} people")}));
            }
            let mut d = crate::catalog::detail(&p, &t, files);
            d["yaml"] = json!(t);
            d["collections"] = json!(crate::catalog::collections().iter().filter(|c| c["plugins"].as_array().map(|x| x.iter().any(|n| n == &json!(name))).unwrap_or(false)).map(|c| c["name"].clone()).collect::<Vec<_>>());
            Ok(d)
        })
        .req("plugin", "catalog name, e.g. birdid")
        .route("GET", "/catalog/{plugin}"),
        Tool::new("install_collection", "Installs the plugins of a collection that this environment does not have yet.", |ctx, a| async move {
            let name = a.need("name")?;
            let c = crate::catalog::collections().into_iter().find(|c| c["name"] == json!(name)).ok_or_else(|| ApiError::not_found(format!("No collection “{name}”")))?;
            let mut added = vec![];
            for n in c["plugins"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str()) {
                if plugin::file(&ctx, n).exists() {
                    continue;
                }
                if let Ok(t) = crate::catalog::text(n) {
                    plugin::save_text(&ctx, n, &t)?;
                    added.push(n.to_string());
                }
            }
            Ok(json!({"collection": name, "added": added}))
        })
        .req("name", "collection name, e.g. Brazil · ICP-Brasil")
        .route("POST", "/catalog/collections/install"),
        Tool::new("import_plugin", "Reads your own plugin from a folder, a .zip or a git URL (path= inside it, ref= branch or tag) and checks it. Installs nothing: install_import does.", |_ctx, a| async move {
            let src = a.need("source")?;
            let (src2, sub) = match src.split_once('#') { Some((u, p)) => (u.to_string(), p.to_string()), None => (src.clone(), a.str("path").unwrap_or_default()) };
            let (sub, rf) = match sub.split_once('@') { Some((p, r)) => (p.to_string(), r.to_string()), None => (sub, a.str("ref").unwrap_or_default()) };
            tokio::task::spawn_blocking(move || crate::catalog::import(&src2, &sub, &rf)).await.map_err(|e| ApiError::internal(e.to_string()))?
        })
        .req("source", "a folder, a .zip, or a git URL (url#path@ref works too)").param("path", "folder inside the repository or zip").param("ref", "branch or tag")
        .route("POST", "/catalog/imports"),
        Tool::new("install_import", "Installs a plugin import_plugin read (only if it had no errors); service= installs it under another name.", |ctx, a| async move {
            let svc = crate::catalog::install_import(&ctx, &a.need("id")?, a.str("service"))?;
            Ok(plugin_view(&ctx, &svc)?)
        })
        .req("id", "the import id").param("service", "another name")
        .route("POST", "/catalog/imports/{id}/install"),
        Tool::new("list_pages", "The pages a service draws (authorize, push approval): fields, buttons, their YAML and a preview URL.", |ctx, a| async move {
            let s = service_of(&a)?;
            let p = plugin::get(&ctx, &s)?;
            let t = plugin::text(&ctx, &s);
            let view = service_view(&ctx, &p).await;
            let base = view["urls"]["local"].as_str().unwrap_or("").to_string();
            let mut d = crate::catalog::detail(&p, &t, vec![]);
            for pg in d["pages"].as_array_mut().into_iter().flatten() {
                pg["preview"] = json!(format!("{base}/__stuntman/preview/{}", pg["index"]));
            }
            Ok(json!({"service": s, "pages": d["pages"], "people": d["people"]}))
        })
        .req("service", "plugin")
        .route("GET", "/plugins/{service}/pages"),
        Tool::new("set_page", "Replaces one page of a service (its YAML: route, title, ask, fields, buttons, checks, push); validated before saving. index= the page (0 first).", |ctx, a| async move {
            let s = service_of(&a)?;
            let mut p = plugin::get(&ctx, &s)?;
            let i = a.u64("index").unwrap_or(0) as usize;
            let y = a.need("yaml")?;
            let page: plugin::Page = serde_yaml::from_str::<Vec<plugin::Page>>(&y).ok().and_then(|v| v.into_iter().next())
                .map(Ok).unwrap_or_else(|| serde_yaml::from_str::<plugin::Page>(&y)).map_err(|e| ApiError::bad(format!("{}", e).replace("unknown variant", "unknown field")))?;
            if i < p.pages.len() { p.pages[i] = page; } else { p.pages.push(page); }
            let v = plugin::save(&ctx, &p)?;
            Ok(json!({"service": s, "version": v}))
        })
        .req("service", "plugin").param("index", "0 = the first page").req("yaml", "the page as YAML")
        .route("PUT", "/plugins/{service}/pages/{index}"),
    ]
}

// ---------------------------------------------------------------- A6 browser steps

async fn step_or_latest(ctx: &Ctx, a: &Args, kind: &str) -> Result<Value, ApiError> {
    if let Some(id) = a.str("step").filter(|s| !s.is_empty()) {
        return crate::steps::get(ctx, &id).await?.ok_or_else(|| ApiError::not_found(format!("No step {id}")));
    }
    crate::steps::list(ctx, a.str("service"), true, 50).await?.into_iter().find(|s| s["kind"] == json!(kind))
        .ok_or_else(|| ApiError::not_found(if kind == "push" { "No push approval is waiting" } else { "No page is waiting: the app has not opened one yet" }))
}

async fn step_view(ctx: &Ctx, s: &Value) -> Value {
    let mut v = s.clone();
    if let Ok(p) = plugin::get(ctx, s["service"].as_str().unwrap_or("")) {
        let base = service_view(ctx, &p).await["urls"]["local"].as_str().unwrap_or("").to_string();
        v["url"] = json!(format!("{base}/__stuntman/steps/{}", s["id"].as_str().unwrap_or("")));
        v["title"] = json!(p.pages.get(s["page"].as_u64().unwrap_or(0) as usize).map(|pg| pg.title.clone()).unwrap_or_default());
        let person = crate::steps::find_person(&p, s["person"].as_str().unwrap_or(""));
        v["person_name"] = person.get("name").cloned().unwrap_or(Value::Null);
    }
    v
}

fn step_tools() -> Vec<Tool> {
    vec![
        Tool::new("pending_browser_steps", "Pages and push approvals waiting for a person (all=true: the recent ones too), with the URL to open them.", |ctx, a| async move {
            let v = crate::steps::list(&ctx, a.str("service"), !a.bool("all").unwrap_or(false), a.u64("limit").unwrap_or(30) as i64).await?;
            let mut out = vec![];
            for s in &v {
                out.push(step_view(&ctx, s).await);
            }
            Ok(json!({"steps": out}))
        })
        .param("service", "plugin").param("all", "include the finished ones").param("limit", "default 30")
        .route("GET", "/apis/steps"),
        Tool::new("authorize_as", "Completes the waiting page as this person (name or CPF), as clicking Authorize; deny=true clicks Deny. Returns where the browser goes (the app's redirect_uri with code and state).", |ctx, a| async move {
            let s = step_or_latest(&ctx, &a, "page").await?;
            let p = plugin::get(&ctx, s["service"].as_str().unwrap_or(""))?;
            let deny = a.bool("deny").unwrap_or(false);
            let mut form = serde_json::Map::new();
            form.insert("action".into(), json!(if deny { "deny" } else { "authorize" }));
            form.insert("person".into(), json!(a.str("person").unwrap_or_default()));
            form.insert("otp".into(), json!(a.str("otp").unwrap_or("123456".into())));
            let o = crate::steps::act(&ctx, &p, s["id"].as_str().unwrap_or(""), &form, &format!("{} via MCP", by(&a)), "").await?;
            let loc = o.headers.iter().find(|(k, _)| k == "location").map(|x| x.1.clone()).unwrap_or_default();
            Ok(json!({"step": s["id"], "result": o.answer, "sentence": o.sentence, "redirect": loc}))
        })
        .param("person", "name or CPF of one of the plugin's people").param("otp", "the code (default 123456; 000000 is refused)").param("deny", "true clicks Deny").param("step", "step id (default: the latest waiting page)").param("service", "plugin")
        .route("POST", "/apis/steps/authorize"),
        Tool::new("approve_push", "The phone side of a push approval: answer=approve (default) | reject | timeout. The app's next poll sees it.", |ctx, a| async move {
            let s = step_or_latest(&ctx, &a, "push").await?;
            let p = plugin::get(&ctx, s["service"].as_str().unwrap_or(""))?;
            let mut form = serde_json::Map::new();
            form.insert("action".into(), json!(a.str("answer").unwrap_or("approve".into())));
            let o = crate::steps::act(&ctx, &p, s["id"].as_str().unwrap_or(""), &form, &format!("{} via MCP", by(&a)), "").await?;
            Ok(json!({"step": s["id"], "result": o.answer, "sentence": o.sentence}))
        })
        .param("answer", "approve | reject | timeout").param("step", "step id (default: the latest waiting push)").param("service", "plugin")
        .route("POST", "/apis/steps/push"),
    ]
}

/// Unix seconds from an ISO date (2026-01-01), date-time in UTC (2026-01-01T12:00:00Z) or an offset
/// from now (-365d, +2h, now).
fn when(s: &str) -> Result<i64, ApiError> {
    let now = util::now_ms() / 1000;
    let s = s.trim();
    let bad = || ApiError::bad(format!("“{s}”: an ISO date (2026-01-01), 2026-01-01T12:00:00Z, or -365d / +2d"));
    if s == "now" {
        return Ok(now);
    }
    if let Some(r) = s.strip_prefix('-').or(s.strip_prefix('+')) {
        let d = util::parse_ms(r).ok_or_else(bad)? as i64 / 1000;
        return Ok(if s.starts_with('-') { now - d } else { now + d });
    }
    let (date, clock) = s.trim_end_matches('Z').split_once('T').unwrap_or((s, "00:00:00"));
    let n: Vec<i64> = date.split('-').chain(clock.split(':')).map(|x| x.parse().ok()).collect::<Option<_>>().ok_or_else(bad)?;
    let [y, mo, d, h, mi, se] = n[..] else { return Err(bad()) };
    let day = time::Date::from_calendar_date(y as i32, time::Month::try_from(mo as u8).map_err(|_| bad())?, d as u8).map_err(|_| bad())?;
    Ok(day.midnight().assume_utc().unix_timestamp() + h * 3600 + mi * 60 + se)
}

fn iso_secs(t: i64) -> String {
    let d = time::OffsetDateTime::from_unix_timestamp(t).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", d.year(), d.month() as u8, d.day(), d.hour(), d.minute(), d.second())
}

#[cfg(test)]
mod when_t {
    #[test]
    fn dates_and_offsets() {
        assert_eq!(super::when("2026-01-01").unwrap(), 1_767_225_600);
        assert_eq!(super::when("2026-01-01T01:00:00Z").unwrap(), 1_767_229_200);
        let now = stuntman_core::util::now_ms() / 1000;
        assert!((super::when("-1d").unwrap() - (now - 86_400)).abs() < 2);
        assert!(super::when("yesterday").is_err() && super::when("2026-13-01").is_err());
    }
}
