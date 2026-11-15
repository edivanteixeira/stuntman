//! N1–N3 · Environments: folders under data/environments/<name>/, each with its own services,
//! state and URLs. Create (empty, a copy of another, or a folder from a repository), pause (503
//! everywhere, state kept), reset to seeds, extend, delete; expired ones are deleted by a sweep.

use std::path::{Path, PathBuf};

use stuntman_core::{
    env::{self, EnvFile, EnvState},
    store::Db,
    tools::Args,
    util, ApiError, Ctx, Tool,
};
use serde_json::{json, Value};

use crate::srv;

/// Tables that hold configuration rather than traffic: reset keeps them (kept e-mail inboxes).
const SEED_TABLES: [&str; 1] = ["email_inboxes"];
const COPY: [&str; 5] = ["services", "state", "scenarios", "inboxes", "secrets"];

pub fn dir(name: &str) -> PathBuf {
    srv().root.join("environments").join(name)
}

fn file_path(name: &str) -> PathBuf {
    dir(name).join("environment.yaml")
}

fn read_file(name: &str) -> Result<(EnvFile, String), ApiError> {
    let text = std::fs::read_to_string(file_path(name)).unwrap_or_default();
    if text.trim().is_empty() {
        return Ok((EnvFile::new(name), String::new()));
    }
    let mut f: EnvFile = serde_yaml::from_str(&text).map_err(|e| ApiError::bad(format!("environments/{name}/environment.yaml: {e}")))?;
    f.name = name.into();
    Ok((f, text))
}

/// A short hash of the file text: the editor sends it back so a change made elsewhere is noticed.
fn version(text: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn write_file(f: &EnvFile) -> Result<(), ApiError> {
    stuntman_core::yamlfile::save(&file_path(&f.name), f)
}

/// Picks up hand edits of environment.yaml (pause, base domain…) for routing.
pub fn refresh(ctx: &Ctx) {
    if let Ok((f, _)) = read_file(&ctx.env) {
        ctx.state.paused.store(f.paused, std::sync::atomic::Ordering::Relaxed);
        *ctx.state.file.write().unwrap() = f;
    }
}

/// Opens an environment folder: database, migrations, router, background work.
pub fn open(name: &str) -> Result<Ctx, ApiError> {
    let s = srv();
    let d = dir(name);
    std::fs::create_dir_all(&d).map_err(|e| ApiError::internal(e.to_string()))?;
    let (f, text) = read_file(name)?;
    if text.is_empty() {
        write_file(&f)?;
    }
    let db = Db::open(&d.join("duble.sqlite")).map_err(|e| ApiError::internal(format!("{name}: {e}")))?;
    let ctx = Ctx {
        env: name.into(),
        state: std::sync::Arc::new(EnvState::new(f)),
        envs: s.envs.clone(),
        db,
        live: stuntman_core::live::Live::default(),
        data_dir: std::sync::Arc::new(d),
        http: s.http.clone(),
        signer: s.signer.clone(),
        host: s.host.clone().into(),
    };
    ctx.db.migrate(stuntman_core::trips::SQL).map_err(|e| ApiError::internal(e.to_string()))?;
    stuntman_core::trips::upgrade(&ctx.db);
    ctx.db.migrate(stuntman_core::usage::SQL).map_err(|e| ApiError::internal(e.to_string()))?;
    {
        // The heartbeat: a gap between two runs' beats is "Stuntman was offline". It also writes the usage counts.
        let c = ctx.clone();
        let start = util::now_ms();
        tokio::spawn(async move {
            loop {
                if stuntman_core::trips::beat(&c, start).await.is_err() { break; }
                let _ = stuntman_core::usage::flush(&c).await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                if !srv().envs.all().iter().any(|e| e.env == c.env) { break; }
            }
        });
    }
    for m in &s.mods {
        if !m.migrations().is_empty() {
            ctx.db.migrate(m.migrations()).map_err(|e| ApiError::internal(format!("{} migrations: {e}", m.id())))?;
        }
    }
    s.envs.insert(ctx.clone());
    s.routers.lock().unwrap().insert(name.to_string(), crate::env_router(ctx.clone()));
    for m in &s.mods {
        m.start(ctx.clone());
    }
    Ok(ctx)
}

/// Moves a pre-environments data folder (flat files) into environments/<name>/.
pub fn migrate_flat(root: &Path, name: &str) {
    if root.join("environments").exists() || !root.join("duble.sqlite").exists() {
        return;
    }
    let to = root.join("environments").join(name);
    std::fs::create_dir_all(&to).expect("environments folder");
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n == "keys" || n == "environments" {
            continue;
        }
        let _ = std::fs::rename(e.path(), to.join(&n));
    }
    println!("Moved the data folder into environments/{name}/");
}

fn find(name: &str) -> Result<Ctx, ApiError> {
    srv().envs.get(name).ok_or_else(|| ApiError::not_found(format!("No environment “{name}”")).with(json!({"environments": names()})))
}

fn names() -> Vec<String> {
    srv().envs.all().iter().map(|c| c.env.to_string()).collect()
}

fn dir_size(p: &Path, skip_db: bool) -> u64 {
    let mut n = 0;
    for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
        let path = e.path();
        if path.is_dir() {
            n += dir_size(&path, skip_db);
        } else if !(skip_db && e.file_name().to_string_lossy().contains(".sqlite")) {
            n += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    n
}

async fn call(ctx: &Ctx, tool: &str, args: Value) -> Value {
    match srv().tools.iter().find(|t| t.name == tool) {
        Some(t) => t.call(ctx.clone(), args).await.unwrap_or(Value::Null),
        None => Value::Null,
    }
}

/// What N1's card and detail, and N3, show.
pub async fn view(ctx: &Ctx) -> Value {
    refresh(ctx);
    let (f, text) = read_file(&ctx.env).unwrap_or((ctx.state.file.read().unwrap().clone(), String::new()));
    let s = srv();
    let services = call(ctx, "list_services", json!({})).await["services"].as_array().cloned().unwrap_or_default();
    let email_inboxes = call(ctx, "list_inboxes", json!({})).await["inboxes"].as_array().map(|a| a.len()).unwrap_or(0);
    let hook_inboxes = call(ctx, "list_webhook_inboxes", json!({})).await["inboxes"].as_array().map(|a| a.len()).unwrap_or(0);
    let hour = util::now_ms() - 3_600_000;
    let (calls, last): (i64, Option<i64>) = ctx
        .db
        .call(move |c| {
            c.query_row(
                "SELECT (SELECT count(*) FROM email_messages WHERE at > ?1) + (SELECT count(*) FROM webhook_deliveries WHERE at > ?1),
                        max((SELECT max(at) FROM email_messages), (SELECT max(at) FROM webhook_deliveries))",
                [hour],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
        })
        .await
        .unwrap_or((0, None));
    let state_bytes = ctx.db.size() + stuntman_telemetry::db(ctx).size();
    let base = f.base_domain.clone();
    let local = format!("http://{}{}", s.host, ctx.prefix());
    let (otlp, _) = stuntman_telemetry::ports();
    let state = if f.paused { "paused" } else { "running" };
    json!({
        "name": f.name, "state": state, "paused": f.paused, "pausedSince": f.paused_since,
        "default": s.envs.default_name() == *ctx.env,
        "lifetime": f.lifetime, "expiresAt": f.expires_at, "createdAt": f.created_at, "createdBy": f.created_by, "clonedFrom": f.cloned_from,
        "baseDomain": base, "realHosts": f.real_hosts, "callbacks": {"default": f.callbacks.default},
        "addresses": {
            "services": format!("*.{base}"),
            "callbacks": f.callbacks.default,
            "mcp": format!("http://{base}:{}/mcp", s.port),
            "api": format!("http://{base}:{}/api", s.port),
            "otlp": format!("http://{base}:{otlp}"),
            "local": local,
            "localMcp": format!("{local}/mcp"),
        },
        "counts": {"services": services.len() + 2, "inboxes": email_inboxes + hook_inboxes, "callsLastHour": calls, "lastCallAt": last, "scenarios": 0},
        "services": services.iter().map(|v| json!({"name": v["service"]["name"], "kind": "email", "callback": v["service"]["events"]["to"]})).collect::<Vec<_>>(),
        "clients": ctx.live.clients(),
        "sizes": {"files": dir_size(&ctx.data_dir, true), "state": state_bytes},
        "file": text, "version": version(&text), "path": format!("environments/{}/environment.yaml", f.name),
    })
}

fn expiry(e: &str) -> Result<(String, Option<i64>), ApiError> {
    let e = e.trim().to_lowercase();
    Ok(match e.as_str() {
        "" | "never" | "kept" => ("kept".into(), None),
        "pr_closed" | "when the pull request closes" | "pr" => ("pr_closed".into(), None),
        d => {
            let ms = util::parse_ms(d.trim_start_matches("in ")).ok_or_else(|| ApiError::bad("expires: never, 1h, 24h, 7d or pr_closed"))?;
            ("expires".into(), Some(util::now_ms() + ms as i64))
        }
    })
}

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(p) = to.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::copy(from, to).map(|_| ())
}

fn copy_tree(from: &Path, to: &Path, skip: &dyn Fn(&Path) -> bool) -> std::io::Result<()> {
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        if skip(&p) {
            continue;
        }
        let t = to.join(e.file_name());
        if p.is_dir() {
            copy_tree(&p, &t, skip)?;
        } else {
            copy_file(&p, &t)?;
        }
    }
    Ok(())
}

/// Ports the copy can't share with its source: each copied SMTP service moves to a free port.
fn free_port(start: u16, taken: &mut Vec<u16>) -> u16 {
    let mut p = start.saturating_add(1);
    // A listener on 127.0.0.1 only doesn't stop a 0.0.0.0 bind on every system: try connecting too.
    let busy = |p: u16| std::net::TcpListener::bind(("0.0.0.0", p)).is_err() || std::net::TcpStream::connect_timeout(&([127, 0, 0, 1], p).into(), std::time::Duration::from_millis(50)).is_ok();
    while taken.contains(&p) || busy(p) {
        p = p.saturating_add(1);
    }
    taken.push(p);
    p
}

fn smtp_ports_in_use() -> Vec<u16> {
    let mut v = vec![];
    for c in srv().envs.all() {
        for e in std::fs::read_dir(c.path("email/services")).into_iter().flatten().flatten() {
            let y: serde_yaml::Value = std::fs::read_to_string(e.path()).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
            if let Some(p) = y["intake"]["smtp"]["port"].as_u64() {
                v.push(p as u16);
            }
        }
    }
    v
}

/// YAML files that don't parse, with their line (creating from a folder).
fn problems(root: &Path) -> Vec<Value> {
    let mut out = vec![];
    fn walk(p: &Path, root: &Path, out: &mut Vec<Value>) {
        for e in std::fs::read_dir(p).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().map(|x| x == "yaml" || x == "yml").unwrap_or(false) {
                let t = std::fs::read_to_string(&path).unwrap_or_default();
                if let Err(err) = serde_yaml::from_str::<serde_yaml::Value>(&t) {
                    let rel = path.strip_prefix(root).unwrap_or(&path).display().to_string();
                    out.push(json!({"file": rel, "line": err.location().map(|l| l.line()), "error": err.to_string()}));
                }
            }
        }
    }
    walk(root, root, &mut out);
    out
}

async fn create(a: Args) -> Result<Value, ApiError> {
    let name = a.need("name")?.trim().to_lowercase();
    if !env::valid_name(&name) {
        return Err(ApiError::bad("Name: lowercase letters, numbers and dashes, like pr-5462"));
    }
    if srv().envs.get(&name).is_some() || dir(&name).exists() {
        return Err(ApiError::new(409, format!("{name} already exists")).with(json!({"name": name, "existing": true})));
    }
    let from = a.str("from").unwrap_or("empty".into());
    let (lifetime, expires_at) = expiry(&a.str("expires").unwrap_or_default())?;
    let copy: Vec<String> = match a.get("copy") {
        Some(_) => a.list("copy").into_iter().map(|c| c.to_lowercase()).collect(),
        None => vec!["services".into(), "scenarios".into(), "inboxes".into(), "secrets".into()],
    };
    if let Some(bad) = copy.iter().find(|c| !COPY.contains(&c.as_str())) {
        return Err(ApiError::bad(format!("copy: {bad}? Use {}", COPY.join(", "))));
    }
    let to = dir(&name);
    let mut f = EnvFile::new(&name);
    f.lifetime = lifetime;
    f.expires_at = expires_at;
    f.created_by = by(&a);
    let mut found = vec![];
    let mut moved = vec![];
    let source = srv().envs.get(&from);
    let from_folder = source.is_none() && from != "empty";
    if let Some(src) = &source {
        f.cloned_from = src.env.to_string();
        let sf = src.state.file.read().unwrap().clone();
        f.real_hosts = vec![];
        f.callbacks = sf.callbacks.clone();
        let has = |k: &str| copy.iter().any(|c| c == k);
        let res: std::io::Result<()> = (|| {
            std::fs::create_dir_all(&to)?;
            if has("services") {
                let mut taken = smtp_ports_in_use();
                for e in std::fs::read_dir(src.path("email/services")).into_iter().flatten().flatten() {
                    let mut y: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(e.path())?).unwrap_or_default();
                    if let Some(p) = y["intake"]["smtp"]["port"].as_u64() {
                        let np = free_port(p as u16, &mut taken);
                        y["intake"]["smtp"]["port"] = np.into();
                        moved.push(json!({"service": y["name"], "from": p, "to": np}));
                    }
                    // The service names its environment and an address domain derived from it.
                    y["env"] = name.clone().into();
                    if let Some(d) = y["addresses"]["domain"].as_str().and_then(|d| d.strip_prefix(&format!("{}.", src.env))).map(|r| format!("{name}.{r}")) {
                        y["addresses"]["domain"] = d.into();
                    }
                    let t = to.join("email/services").join(e.file_name());
                    std::fs::create_dir_all(t.parent().unwrap())?;
                    std::fs::write(t, serde_yaml::to_string(&y).unwrap_or_default())?;
                }
                // Storage: accounts, rules, subscriptions and the seed folders.
                for d in ["storage", "seed", "apis"] {
                    if src.path(d).is_dir() {
                        copy_tree(&src.path(d), &to.join(d), &|_| false)?;
                    }
                }
                if src.path("email/extractors.yaml").exists() {
                    copy_file(&src.path("email/extractors.yaml"), &to.join("email/extractors.yaml"))?;
                }
                let tel = src.path(&format!("telemetry/{}.yaml", src.env));
                if tel.exists() {
                    let mut y: serde_yaml::Value = serde_yaml::from_str(&std::fs::read_to_string(&tel)?).unwrap_or_default();
                    y["environment"] = name.clone().into();
                    std::fs::create_dir_all(to.join("telemetry"))?;
                    std::fs::write(to.join(format!("telemetry/{name}.yaml")), serde_yaml::to_string(&y).unwrap_or_default())?;
                }
            }
            if has("inboxes") && src.path("webhooks/inboxes.yaml").exists() {
                copy_file(&src.path("webhooks/inboxes.yaml"), &to.join("webhooks/inboxes.yaml"))?;
            }
            if has("secrets") && src.path("secrets.yaml").exists() {
                copy_file(&src.path("secrets.yaml"), &to.join("secrets.yaml"))?;
            }
            Ok(())
        })();
        res.map_err(|e| ApiError::internal(format!("copying {}: {e}", src.env)))?;
        if has("state") {
            let (a, b) = (src.db.clone(), stuntman_telemetry::db(src));
            let (p1, p2) = (to.join("duble.sqlite"), to.join("telemetry.sqlite"));
            tokio::task::spawn_blocking(move || a.copy_to(&p1).and_then(|_| b.copy_to(&p2)))
                .await
                .map_err(|e| ApiError::internal(e.to_string()))?
                .map_err(|e| ApiError::internal(format!("copying state: {e}")))?;
        }
    } else if from_folder {
        let p = PathBuf::from(&from);
        if !p.is_dir() {
            return Err(ApiError::bad(format!("from: an environment ({}), \"empty\" or a folder; “{from}” is none of them", names().join(", "))));
        }
        copy_tree(&p, &to, &|x: &Path| {
            let n = x.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            n.contains(".sqlite") || n == "environment.yaml"
        })
        .map_err(|e| ApiError::internal(format!("copying {from}: {e}")))?;
        if let Ok(t) = std::fs::read_to_string(p.join("environment.yaml")) {
            if let Ok(g) = serde_yaml::from_str::<EnvFile>(&t) {
                f.callbacks = g.callbacks;
                f.real_hosts = g.real_hosts;
            }
        }
        f.cloned_from = from.clone();
        found = problems(&to);
    }
    if let Some(c) = a.get("callbacks") {
        f.callbacks.default = c.get("default").and_then(|v| v.as_str()).or(c.as_str()).unwrap_or("").to_string();
    }
    write_file(&f)?;
    let ctx = open(&name)?;
    if let (Some(src), true) = (&source, copy.iter().any(|c| c == "inboxes") && !copy.iter().any(|c| c == "state")) {
        let p = src.path("duble.sqlite").to_string_lossy().to_string();
        ctx.db
            .call(move |c| c.execute_batch(&format!("ATTACH DATABASE '{}' AS src; INSERT OR IGNORE INTO email_inboxes SELECT * FROM src.email_inboxes WHERE kind='kept'; DETACH DATABASE src;", p.replace('\'', "''"))))
            .await?;
    }
    for c in srv().envs.all() {
        c.live.send("environments", &name, "created");
    }
    let mut v = view(&ctx).await;
    v["problems"] = json!(found);
    v["smtpPortsMoved"] = json!(moved);
    v["copied"] = json!(if source.is_some() { copy } else { vec![] });
    Ok(v)
}

async fn set(a: Args) -> Result<Value, ApiError> {
    let name = a.need("name")?;
    let ctx = find(&name)?;
    let (mut f, text) = read_file(&ctx.env)?;
    if let Some(v) = a.str("version") {
        if v != version(&text) {
            let at = std::fs::metadata(file_path(&ctx.env)).and_then(|m| m.modified()).ok().map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64);
            return Err(ApiError::new(409, "environment.yaml changed elsewhere").with(json!({"file": text, "version": version(&text), "changedAt": at})));
        }
    }
    if let Some(b) = a.str("base_domain") {
        let b = b.trim().trim_end_matches('.').to_lowercase();
        if b.is_empty() || b.contains('/') || b.contains(' ') {
            return Err(ApiError::bad("base_domain: a host name like pr-5462.stuntman.local"));
        }
        if let Some(o) = srv().envs.all().into_iter().find(|c| c.env != ctx.env && c.state.file.read().unwrap().base_domain == b) {
            return Err(ApiError::new(409, format!("{} already uses {b}", o.env)));
        }
        f.base_domain = b;
    }
    if let Some(c) = a.get("callbacks") {
        f.callbacks.default = c.get("default").and_then(|v| v.as_str()).or(c.as_str()).unwrap_or("").trim().to_string();
        if !f.callbacks.default.is_empty() && !f.callbacks.default.starts_with("http://") && !f.callbacks.default.starts_with("https://") {
            return Err(ApiError::bad("callbacks.default: a URL starting with http:// or https://"));
        }
    }
    if a.get("real_hosts").is_some() {
        f.real_hosts = a.list("real_hosts");
    }
    if let Some(l) = a.str("lifetime").or(a.str("expires")) {
        let (lt, at) = expiry(&l)?;
        f.lifetime = lt;
        f.expires_at = at;
    }
    if let Some(p) = a.bool("paused") {
        pause_file(&mut f, p);
    }
    save(&ctx, f)?;
    Ok(view(&ctx).await)
}

fn pause_file(f: &mut EnvFile, p: bool) {
    if p != f.paused {
        f.paused_since = p.then(util::now_ms);
    }
    f.paused = p;
}

fn save(ctx: &Ctx, f: EnvFile) -> Result<(), ApiError> {
    write_file(&f)?;
    ctx.state.paused.store(f.paused, std::sync::atomic::Ordering::Relaxed);
    *ctx.state.file.write().unwrap() = f;
    for c in srv().envs.all() {
        c.live.send("environments", &ctx.env, "updated");
    }
    Ok(())
}

pub async fn delete(name: &str) -> Result<Value, ApiError> {
    let ctx = find(name)?;
    if srv().envs.all().len() <= 1 {
        return Err(ApiError::new(409, format!("{} is the only environment; make another one first", ctx.env)));
    }
    ctx.state.gone.store(true, std::sync::atomic::Ordering::Relaxed);
    srv().envs.remove(&ctx.env);
    srv().routers.lock().unwrap().remove(&*ctx.env);
    for m in &srv().mods {
        m.stop(&ctx);
    }
    ctx.live.send("environments", &ctx.env, "deleted");
    for c in srv().envs.all() {
        c.live.send("environments", &ctx.env, "deleted");
    }
    let d = ctx.data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(d)).await.map_err(|e| ApiError::internal(e.to_string()))?.map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(json!({"deleted": name, "environments": names()}))
}

/// Deletes expired environments and reloads hand-edited files. Runs every 30 s.
pub async fn sweep() {
    for c in srv().envs.all() {
        refresh(&c);
        let f = c.state.file.read().unwrap().clone();
        if f.lifetime == "expires" && f.expires_at.map(|t| t < util::now_ms()).unwrap_or(false) {
            match delete(&c.env).await {
                Ok(_) => println!("environment {} expired and was deleted", c.env),
                Err(e) => eprintln!("environment {} expired: {}", c.env, e.message),
            }
        }
    }
}

fn hosts(ctx: &Ctx) -> String {
    let f = ctx.state.file.read().unwrap().clone();
    let b = &f.base_domain;
    let mut lines = vec![format!("# Stuntman · {} · add to /etc/hosts (or a DNS zone: *.{b} → this machine)", f.name)];
    for h in [b.clone(), format!("smtp.{b}"), format!("sendgrid.{b}"), format!("hooks.{b}"), format!("otlp.{b}")] {
        lines.push(format!("127.0.0.1  {h}"));
    }
    for a in stuntman_storage::config::list(ctx).into_iter().filter(|a| a.kind == stuntman_storage::config::AZURE) {
        lines.push(format!("127.0.0.1  {}.blob.{b}", a.account));
    }
    for h in &f.real_hosts {
        lines.push(format!("127.0.0.1  {h}   # real host name, answered with the Stuntman Test CA"));
    }
    lines.join("\n") + "\n"
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_environments", "Environments on this Stuntman: name, state, lifetime, services, addresses.", |_ctx, _a| async move {
            let mut out = vec![];
            for c in srv().envs.all() {
                out.push(view(&c).await);
            }
            // The default one first, then kept ones, then those that expire; by name within.
            out.sort_by_key(|v| (v["default"] != true, v["lifetime"] == "expires", v["name"].as_str().unwrap_or("").to_string()));
            Ok(json!({"environments": out, "default": srv().envs.default_name()}))
        })
        .route("GET", "/environments"),
        Tool::new("get_environment", "One environment: addresses, callbacks, real hosts, lifetime, paused, environment.yaml.", |_ctx, a| async move {
            Ok(view(&find(&a.need("name")?)?).await)
        })
        .req("name", "environment name, e.g. stg-a")
        .route("GET", "/environments/{name}"),
        Tool::new(
            "create_environment",
            "Make an environment: from another one (a copy), \"empty\", or a folder; returns its addresses. copy picks what a copy takes: services, state, scenarios, inboxes, secrets (default: all but state).",
            |_ctx, a| create(a),
        )
        .req("name", "lowercase, numbers and dashes; becomes <name>.stuntman.local")
        .param("from", "an environment name, \"empty\" (default) or a folder environments/<name> from a repository")
        .param("copy", "list: services, state, scenarios, inboxes, secrets")
        .param("expires", "never (default), 1h, 24h, 7d or pr_closed")
        .param("callbacks", "{\"default\": \"https://…\"}: where services send webhooks")
        .route("POST", "/environments"),
        Tool::new("set_environment", "Change callbacks, lifetime, paused, base_domain or real_hosts (same keys as environment.yaml).", |_ctx, a| set(a))
            .req("name", "environment name")
            .param("callbacks", "{\"default\": \"http://host.docker.internal:5000\"}")
            .param("lifetime", "kept, 1h, 24h, 7d or pr_closed")
            .param("paused", "true: every service answers 503, state kept")
            .param("base_domain", "e.g. stg-a.stuntman.local")
            .param("real_hosts", "list of real provider host names answered with the Stuntman Test CA")
            .param("version", "the version you read; a change made elsewhere since then answers 409")
            .route("PUT", "/environments/{name}"),
        Tool::new("pause_environment", "Pause (every service answers 503, nothing is stored, state kept) or resume.", |_ctx, a| async move {
            let ctx = find(&a.need("name")?)?;
            let (mut f, _) = read_file(&ctx.env)?;
            pause_file(&mut f, a.bool("paused").unwrap_or(true));
            save(&ctx, f)?;
            Ok(view(&ctx).await)
        })
        .req("name", "environment name")
        .param("paused", "true (default) or false to resume")
        .route("POST", "/environments/{name}/pause"),
        Tool::new("extend_environment", "Push an expiring environment's expiry (default 24 h from its current expiry or now).", |_ctx, a| async move {
            let ctx = find(&a.need("name")?)?;
            let by = a.ms("by")?.unwrap_or(86_400_000) as i64;
            let (mut f, _) = read_file(&ctx.env)?;
            f.expires_at = Some(f.expires_at.unwrap_or(0).max(util::now_ms()) + by);
            f.lifetime = "expires".into();
            save(&ctx, f)?;
            Ok(view(&ctx).await)
        })
        .req("name", "environment name")
        .param("by", "duration, default 24h")
        .route("POST", "/environments/{name}/extend"),
        Tool::new("reset_environment", "Every service back to its seeds: traffic, deliveries, telemetry and temporary inboxes go; files and kept inboxes stay.", |_ctx, a| async move {
            let ctx = find(&a.need("name")?)?;
            let (d, t) = (ctx.db.clone(), stuntman_telemetry::db(&ctx));
            tokio::task::spawn_blocking(move || {
                d.wipe(&SEED_TABLES)?;
                d.migrate("DELETE FROM email_inboxes WHERE kind != 'kept'")?;
                t.wipe(&[])
            })
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .map_err(|e| ApiError::internal(e.to_string()))?;
            for m in &srv().mods {
                m.reset(&ctx);
            }
            ctx.live.send("environments", &ctx.env, "reset");
            Ok(view(&ctx).await)
        })
        .req("name", "environment name")
        .route("POST", "/environments/{name}/reset"),
        Tool::new("delete_environment", "Delete an environment: services, state and history. Irreversible.", |_ctx, a| async move { delete(&a.need("name")?).await })
            .req("name", "environment name")
            .route("DELETE", "/environments/{name}"),
        Tool::new("get_environment_hosts", "Lines for /etc/hosts (or a DNS zone) so the environment's host names reach this Stuntman.", |_ctx, a| async move {
            let ctx = find(&a.need("name")?)?;
            Ok(json!({"hosts": hosts(&ctx)}))
        })
        .req("name", "environment name")
        .route("GET", "/environments/{name}/hosts"),
    ]
}
