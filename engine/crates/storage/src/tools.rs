//! Storage tools (S1–S5 "API & MCP" sheets): one vocabulary for Azure Blob and S3 — account,
//! container (bucket), path. Everything the screens do, an agent or a test does too.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};
use serde_json::{json, Value};

use crate::{
    auth,
    config::{self, Account},
    events, links, ops, rules, store,
};

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

/// "you" for the screen, "claude-code" for "mcp:claude-code", "qa-suite" for "api:qa-suite".
fn who(a: &Args) -> String {
    by(a).trim_start_matches("mcp:").trim_start_matches("api:").to_string()
}

fn via(a: &Args) -> &'static str {
    let b = by(a);
    if b.starts_with("mcp:") { "mcp" } else if b.starts_with("api:") { "api" } else { "ui" }
}

/// The service: "service" (MCP) or the {name} of /services/{name}/… (HTTP).
fn service(a: &Args) -> Result<String, ApiError> {
    let s = a.str("service").or(a.str("name")).ok_or_else(|| ApiError::bad("“service” is required: azure-blob or s3"))?;
    if s != config::AZURE && s != config::S3 {
        return Err(ApiError::not_found(format!("No storage service “{s}”: azure-blob or s3")));
    }
    Ok(s)
}

/// The rule's name: "rule" over HTTP (the path's {name} is the service), "name" over MCP.
fn rule_name(a: &Args) -> Option<String> {
    if a.str("service").is_some() { a.str("rule").or(a.str("name")) } else { a.str("rule") }
}

pub async fn account_view(ctx: &Ctx, a: &Account) -> Value {
    let counts = store::counts(ctx, &a.account).await.unwrap_or_default();
    let (objects, bytes) = counts.values().fold((0, 0), |s, x| (s.0 + x.0, s.1 + x.1));
    let keys: Vec<Value> = a.access.keys.iter().map(|k| json!({"name": k, "masked": config::mask(&config::key_value(ctx, &a.account, k))})).collect();
    let seed = store::seed_info(ctx, &a.account).await.ok().flatten();
    let changes = match seed { Some((at, _)) => store::changes_since(ctx, &a.account, at).await.ok(), None => None };
    let (text, etag) = config::text(ctx, &a.account);
    json!({
        "account": a.account, "kind": a.kind, "settings": a, "endpoint": ops::endpoint(ctx, a), "connection": ops::connection(ctx, a),
        "containers": a.containers.iter().map(|(n, c)| json!({"name": n, "access": c.access, "objects": counts.get(n).map(|x| x.0).unwrap_or(0), "bytes": counts.get(n).map(|x| x.1).unwrap_or(0)})).collect::<Vec<_>>(),
        "objects": objects, "bytes": bytes, "keys": keys, "reachability": store::reach(ctx, &a.account).await.ok().flatten(),
        "seed": {"path": a.seed, "files": seed.map(|s| s.1), "loadedAt": seed.map(|s| s.0), "exists": ctx.path(&a.seed).is_dir(),
                 "changes": changes.map(|(app, people, del)| json!({"app": app, "people": people, "deletes": del, "total": app + people + del}))},
        "file": config::rel_file(&a.account), "yaml": text, "etag": etag,
    })
}

fn object_view(b: &store::Blob, reads: i64, next: Option<&String>) -> Value {
    let mut v = json!(b);
    v["name"] = json!(b.path.rsplit('/').next().unwrap_or(&b.path));
    v["reads"] = json!(reads);
    v["nextRead"] = json!(next);
    v
}

fn by_filter(b: &store::Blob, f: &str) -> bool {
    match f {
        "" => true,
        "you" => b.via == "ui",
        "seed" => b.via == "seed",
        "agent" => b.via == "mcp" || b.via == "api",
        "app" => !["ui", "seed", "mcp", "api"].contains(&b.via.as_str()),
        other => b.by == other,
    }
}

fn content_of(a: &Args) -> Result<Vec<u8>, ApiError> {
    if let Some(b) = a.str("content_base64") {
        return B64.decode(b.trim()).map_err(|e| ApiError::bad(format!("content_base64: {e}")));
    }
    if let Some(f) = a.str("file") {
        return std::fs::read(&f).map_err(|e| ApiError::bad(format!("file {f}: {e}")));
    }
    Ok(a.str("content").unwrap_or_default().into_bytes())
}

/// Wipes an account's objects and loads its seed folder: <seed>/<container>/<path>.
pub async fn load_seed(ctx: &Ctx, a: &Account) -> Result<i64, ApiError> {
    let acct = a.account.clone();
    ctx.db.call(move |c| store::wipe_objects(c, &acct)).await?;
    let root = ctx.path(&a.seed);
    let mut n = 0;
    for (container, _) in &a.containers {
        let base = root.join(container);
        let mut stack = vec![base.clone()];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                    continue;
                }
                let rel = p.strip_prefix(&base).unwrap_or(&p).to_string_lossy().replace('\\', "/");
                let data = std::fs::read(&p).unwrap_or_default();
                let ctype = mime(&rel);
                ops::write(ctx, a, ops::Write { container: container.clone(), path: rel, data, ctype, meta: BTreeMap::new(), headers: BTreeMap::new(), by: "seed".into(), via: "seed".into(), etag: None, truncated: false }).await?;
                n += 1;
            }
        }
    }
    store::set_seed(ctx, &a.account, n).await?;
    ctx.live.send("storage.object", &a.account, "reset");
    Ok(n)
}

pub fn mime(p: &str) -> String {
    let ext = p.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "txt" | "log" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "html" => "text/html",
        _ => "application/octet-stream",
    }
    .into()
}

pub fn all() -> Vec<Tool> {
    let mut v = objects();
    v.extend(links_tools());
    v.extend(events_tools());
    v.extend(rules_tools());
    v.extend(accounts());
    v
}

// ---------------------------------------------------------------- S1 objects

fn objects() -> Vec<Tool> {
    vec![
        Tool::new("list_storage_accounts", "Storage accounts (Azure Blob and S3) with containers, counts, endpoint, connection string, reachability and seed.", |ctx, _a| async move {
            let mut out = vec![];
            for a in config::list(&ctx) {
                out.push(account_view(&ctx, &a).await);
            }
            Ok(json!({"accounts": out, "ports": {"azureBlob": crate::blob_port(), "s3": crate::s3_port()}}))
        })
        .route("GET", "/storage/accounts"),
        Tool::new("list_objects", "Objects and folders under a prefix (one level), newest first; by = app, you, seed, agent or a caller name.", |ctx, a| async move {
            let (acct, cont) = (a.need("account")?, a.need("container")?);
            let acc = ops::container_of(&ctx, &acct, &cont)?;
            let prefix = a.str("prefix").unwrap_or_default();
            let f = a.str("by").unwrap_or_default();
            let all = store::list(&ctx, &acct, &cont, &prefix).await?;
            let mut folders: BTreeMap<String, (i64, i64)> = BTreeMap::new();
            let mut files = vec![];
            for b in all.into_iter().filter(|b| by_filter(b, &f)) {
                let rest = &b.path[prefix.len()..];
                match rest.split_once('/') {
                    Some((d, _)) => {
                        let e = folders.entry(d.to_string()).or_default();
                        e.0 += 1;
                        e.1 = e.1.max(b.at);
                    }
                    None => files.push(b),
                }
            }
            files.sort_by(|x, y| y.at.cmp(&x.at));
            let reads = store::reads(&ctx, &acct, &cont, files.iter().map(|b| b.path.clone()).collect()).await?;
            let nexts = store::nexts(&ctx, &acct, &cont).await?;
            let total = store::counts(&ctx, &acct).await?.get(&cont).cloned().unwrap_or((0, 0));
            Ok(json!({
                "account": acct, "container": cont, "prefix": prefix, "kind": acc.kind, "totalObjects": total.0, "totalBytes": total.1,
                "folders": folders.into_iter().map(|(n, (c, at))| json!({"name": n, "path": format!("{prefix}{n}/"), "objects": c, "at": at})).collect::<Vec<_>>(),
                "objects": files.iter().map(|b| object_view(b, reads.get(&b.path).copied().unwrap_or(0), nexts.get(&b.path))).collect::<Vec<_>>(),
            }))
        })
        .req("account", "account name").req("container", "container or bucket").param("prefix", "folder, e.g. 2026/11/").param("by", "app | you | seed | agent | a caller")
        .route("GET", "/storage/{account}/{container}/objects"),
        Tool::new("get_object", "An object: properties, metadata, versions, the next read armed on it and its history (requests, links, events).", |ctx, a| async move {
            let (acct, cont, path) = (a.need("account")?, a.need("container")?, a.need("path")?);
            let acc = ops::container_of(&ctx, &acct, &cont)?;
            let b = store::get(&ctx, &acct, &cont, &path, false).await?.ok_or_else(|| ApiError::not_found(format!("No object {cont}/{path}")))?;
            let versions = store::versions(&ctx, &acct, &cont, &path).await?;
            let hist = store::requests_of(&ctx, &acct, &cont, &path).await?;
            let p2 = path.clone();
            let evs = events::list(&ctx, None, None, 500).await?.into_iter().filter(|e| e["container"] == json!(cont) && e["path"] == json!(p2)).collect::<Vec<_>>();
            let lk = links::list(&ctx).await?.into_iter().filter(|l| l["container"] == json!(cont) && l["path"] == json!(path)).collect::<Vec<_>>();
            let next = store::next_of(&ctx, &acct, &cont, &path).await?;
            let reads = hist.iter().filter(|r| r["op"] == "get" && r["status"].as_i64().unwrap_or(0) < 300).count();
            let mut v = object_view(&b, reads as i64, None);
            v["nextRead"] = json!(next.map(|(r, t, by)| json!({"result": r, "times": t, "by": by})));
            v["versions"] = json!(versions.iter().map(|x| json!({"seq": x.seq, "etag": x.etag, "size": x.size, "at": x.at, "by": x.by, "md5": x.md5})).collect::<Vec<_>>());
            v["history"] = json!(hist);
            v["events"] = json!(evs.iter().map(|e| json!({"id": e["id"], "type": e["short"], "subscription": e["subscription"], "status": e["status"], "at": e["at"], "by": e["by"]})).collect::<Vec<_>>());
            v["links"] = json!(lk);
            v["url"] = json!(ops::object_url(&ctx, &acc, &cont, &path));
            v["contentUrl"] = json!(format!("/api/storage/{acct}/{cont}/content/{}", auth::aws_encode(&path, true)));
            Ok(v)
        })
        .req("account", "account").req("container", "container").req("path", "path inside the container")
        .route("GET", "/storage/{account}/{container}/objects/{*path}"),
        Tool::new("wait_for_object", "Waits for the first new object under a prefix (what an agent waits for after the app uploads).", |ctx, a| async move {
            let (acct, cont) = (a.need("account")?, a.need("container")?);
            ops::container_of(&ctx, &acct, &cont)?;
            let prefix = a.str("prefix").unwrap_or_default();
            let since = a.get("since").and_then(|v| v.as_i64()).unwrap_or(util::now_ms() - 2000);
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            loop {
                if let Some(b) = store::list(&ctx, &acct, &cont, &prefix).await?.into_iter().filter(|b| b.at >= since).max_by_key(|b| b.at) {
                    return Ok(object_view(&b, 0, None));
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No new object under {cont}/{prefix} yet")).with(json!({"waitedMs": a.ms("timeout")?.unwrap_or(30_000)})));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .req("account", "account").req("container", "container").param("prefix", "path prefix").param("timeout", "default 30s").param("since", "ms; default: 2 s ago")
        .route("GET", "/storage/{account}/{container}/wait"),
        Tool::new("put_object", "Adds or replaces an object, recorded as added by the caller (never as the app). content (text), content_base64 or file (a local path).", |ctx, a| async move {
            let (acct, cont, path) = (a.need("account")?, a.need("container")?, a.need("path")?);
            let acc = ops::container_of(&ctx, &acct, &cont)?;
            let data = content_of(&a)?;
            let size = data.len() as i64;
            let ctype = a.str("content_type").unwrap_or_else(|| mime(&path));
            let truncated = a.bool("truncated").unwrap_or(false);
            let b = ops::write(&ctx, &acc, ops::Write { container: cont.clone(), path: path.clone(), data, ctype: ctype.clone(), meta: BTreeMap::new(), headers: BTreeMap::new(), by: who(&a), via: via(&a).into(), etag: None, truncated }).await?;
            let seq = store::record(&ctx, store::Request { account: acct.clone(), container: cont.clone(), path: path.clone(), op: "put".into(), method: "PUT".into(), status: 201, caller: who(&a), via: via(&a).into(), size, ctype, detail: if truncated { "cut off on purpose".into() } else { String::new() }, ..Default::default() }).await?;
            ops::emit(&ctx, &acc, &b, true, "PutBlob", &who(&a), Some(seq)).await;
            Ok(object_view(&b, 0, None))
        })
        .req("account", "account").req("container", "container").req("path", "path").param("content", "text").param("content_base64", "bytes as base64").param("file", "a local file path")
        .param("content_type", "default: from the extension").param("truncated", "true: mark it as cut off on purpose")
        .route("PUT", "/storage/{account}/{container}/objects/{*path}"),
        Tool::new("delete_object", "Deletes an object; the delete stays in the history and sends BlobDeleted.", |ctx, a| async move {
            let (acct, cont, path) = (a.need("account")?, a.need("container")?, a.need("path")?);
            let acc = ops::container_of(&ctx, &acct, &cont)?;
            let b = ops::delete(&ctx, &acc, &cont, &path).await?.ok_or_else(|| ApiError::not_found(format!("No object {cont}/{path}")))?;
            let seq = store::record(&ctx, store::Request { account: acct, container: cont, path, op: "delete".into(), method: "DELETE".into(), status: 202, caller: who(&a), via: via(&a).into(), ..Default::default() }).await?;
            ops::emit(&ctx, &acc, &b, false, "DeleteBlob", &who(&a), Some(seq)).await;
            Ok(json!({"deleted": true}))
        })
        .req("account", "account").req("container", "container").req("path", "path")
        .route("DELETE", "/storage/{account}/{container}/objects/{*path}"),
        Tool::new("set_next_read", "Arms how the next read(s) of an object answer: 403 | 404 | 503 | slow:5s | truncate | bad_md5 | normal. Once by default, then back to normal.", |ctx, a| async move {
            let (acct, cont, path) = (a.need("account")?, a.need("container")?, a.need("path")?);
            ops::container_of(&ctx, &acct, &cont)?;
            let r = a.need("result")?;
            if !["403", "404", "503", "truncate", "bad_md5", "normal"].contains(&r.as_str()) && !r.starts_with("slow") {
                return Err(ApiError::bad("result: 403 | 404 | 503 | slow:5s | truncate | bad_md5 | normal"));
            }
            let r = if r == "slow" { "slow:5s".to_string() } else { r };
            let times = a.u64("times").unwrap_or(1) as i64;
            store::set_next(&ctx, &acct, &cont, &path, &r, times, &by(&a)).await?;
            ctx.live.send("storage.object", &format!("{acct}/{cont}/{path}"), "updated");
            Ok(json!({"path": path, "nextRead": if r == "normal" { Value::Null } else { json!({"result": r, "times": times}) }}))
        })
        .req("account", "account").req("container", "container").req("path", "path").req("result", "403 | 404 | 503 | slow:5s | truncate | bad_md5 | normal").param("times", "default 1")
        .route("PUT", "/storage/{account}/{container}/next-read/{*path}"),
        Tool::new("set_account_reachability", "Makes a whole account unreachable: timeout (no answer), 503 or 401; ok brings it back.", |ctx, a| async move {
            let acct = a.need("account")?;
            config::get(&ctx, &acct)?;
            let mode = a.need("mode")?;
            if !["ok", "timeout", "503", "401"].contains(&mode.as_str()) {
                return Err(ApiError::bad("mode: ok | timeout | 503 | 401"));
            }
            store::set_reach(&ctx, &acct, &mode, &by(&a)).await?;
            ctx.live.send("storage.account", &acct, "updated");
            Ok(json!({"account": acct, "mode": mode}))
        })
        .req("account", "account").req("mode", "ok | timeout | 503 | 401")
        .route("PUT", "/storage/{account}/reachability"),
        Tool::new("reset_to_seed", "Every storage account (or the ones named) back to its seed folder; says what changed since the seed was loaded.", |ctx, a| async move {
            let names = a.list("accounts");
            let mut out = vec![];
            for acc in config::list(&ctx).into_iter().filter(|x| names.is_empty() || names.contains(&x.account)) {
                let before = match store::seed_info(&ctx, &acc.account).await? { Some((at, _)) => store::changes_since(&ctx, &acc.account, at).await?, None => (0, 0, 0) };
                let n = load_seed(&ctx, &acc).await?;
                out.push(json!({"account": acc.account, "lost": {"app": before.0, "people": before.1, "deletes": before.2}, "loaded": n}));
            }
            Ok(json!({"accounts": out}))
        })
        .param("accounts", "list; default every account").param("to", "seed (the only target)")
        .route("POST", "/storage/reset"),
    ]
}

// ---------------------------------------------------------------- S3 links

fn link_view(l: &Value) -> Value {
    let mut v = l.clone();
    v["state"] = json!(links::state(l));
    v
}

fn links_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_links", "Links (Azure SAS, S3 presigned) with object, what they allow, validity, uses and refusals. state = valid | expired | revoked.", |ctx, a| async move {
            let (st, issued, obj, kind) = (a.str("state"), a.str("issued_by"), a.str("object"), a.str("kind"));
            let all: Vec<Value> = links::list(&ctx).await?.iter().map(link_view).collect();
            let shown: Vec<&Value> = all.iter().filter(|l| st.as_ref().map(|s| l["state"] == json!(s)).unwrap_or(true) && issued.as_ref().map(|s| l["issuedBy"].as_str().unwrap_or("").contains(s.as_str())).unwrap_or(true)
                && obj.as_ref().map(|s| l["path"].as_str().unwrap_or("").contains(s.as_str())).unwrap_or(true) && kind.as_ref().map(|s| l["kind"] == json!(s)).unwrap_or(true)).collect();
            let count = |f: &dyn Fn(&Value) -> bool| all.iter().filter(|l| f(l)).count();
            let mut issuers: BTreeMap<String, usize> = BTreeMap::new();
            for l in &all {
                *issuers.entry(l["issuedBy"].as_str().unwrap_or("").to_string()).or_default() += 1;
            }
            Ok(json!({"links": shown, "counts": {"all": all.len(), "valid": count(&|l| l["state"] == "valid"), "expired": count(&|l| l["state"] == "expired"), "revoked": count(&|l| l["state"] == "revoked"),
                "sas": count(&|l| l["kind"] == "sas"), "presigned": count(&|l| l["kind"] == "presigned"), "issuedBy": issuers}}))
        })
        .param("state", "valid | expired | revoked").param("issued_by", "who issued it").param("object", "part of the path").param("kind", "sas | presigned")
        .route("GET", "/storage/links"),
        Tool::new("get_link", "What a link allows, its full URL and every use with the answer given.", |ctx, a| async move {
            let id = a.need("id")?;
            let mut l = link_view(&links::get(&ctx, &id).await?);
            l["uses"] = json!(links::uses(&ctx, &id).await?);
            Ok(l)
        })
        .req("id", "link id, e.g. lnk_3f2c9e1a")
        .route("GET", "/storage/links/{id}"),
        Tool::new("wait_for_link", "Waits for the first link to an object under a prefix (the app's own SAS shows up at its first use); returns the URL.", |ctx, a| async move {
            let (acct, cont) = (a.need("account")?, a.need("container")?);
            let since = a.get("since").and_then(|v| v.as_i64()).unwrap_or(util::now_ms() - 2000);
            match links::wait_for(&ctx, &acct, &cont, &a.str("prefix").unwrap_or_default(), since, a.ms("timeout")?.unwrap_or(30_000)).await? {
                Some(l) => Ok(link_view(&l)),
                None => Err(ApiError::new(408, "No link for that object yet")),
            }
        })
        .req("account", "account").req("container", "container").param("prefix", "path prefix").param("timeout", "default 30s").param("since", "ms; default 2 s ago")
        .route("GET", "/storage/{account}/{container}/links/wait"),
        Tool::new("create_link", "A URL signed by Stuntman (Azure SAS or S3 presigned, by the account's kind), recorded as created by the caller. allows: read, write, delete, list.", |ctx, a| async move {
            let (acct, cont, path) = (a.need("account")?, a.need("container")?, a.need("path")?);
            let acc = ops::container_of(&ctx, &acct, &cont)?;
            let mut allows = a.list("allows");
            if allows.is_empty() {
                allows = vec!["read".into()];
            }
            let secs = a.ms("valid_for")?.or(a.ms("expires_in")?).unwrap_or(3_600_000) as i64 / 1000;
            let ips = a.str("ips").unwrap_or_default();
            let now = util::now_ms();
            let key = acc.access.keys.first().cloned().unwrap_or_default();
            let secret = config::key_value(&ctx, &acct, &key);
            let base = ops::object_url(&ctx, &acc, &cont, &path);
            let (kind, q) = if acc.kind == config::AZURE {
                let sp: String = [("read", 'r'), ("write", 'w'), ("delete", 'd'), ("list", 'l')].iter().filter(|(w, _)| allows.iter().any(|x| x == w)).map(|(_, c)| *c).collect();
                ("sas", auth::make_azure_sas(&acct, &cont, &path, &sp, now - 60_000, now + secs * 1000, &ips, &secret))
            } else {
                let host = base.split("://").nth(1).and_then(|r| r.split('/').next()).unwrap_or("localhost").to_string();
                let raw = format!("{}/{cont}/{}", ctx.prefix(), auth::aws_encode(&path, true));
                let m = if allows.iter().any(|x| x == "write") { "PUT" } else { "GET" };
                ("presigned", auth::make_presigned(m, &host, &raw, &key, &secret, now, secs))
            };
            let url = format!("{base}?{q}");
            let (acct2, cont2, path2) = (acct.clone(), cont.clone(), path.clone());
            let sig = auth::query(&q).into_iter().find(|(k, _)| k == "sig" || k == "X-Amz-Signature").map(|x| x.1).unwrap_or_default();
            let s = links::Seen { kind, sig, account: acct, container: cont, path, allows: allows.join(","), start: Some(now - 60_000), expiry: now + secs * 1000, ips, protocol: if kind == "sas" { "https,http".into() } else { String::new() }, signed_with: key, url: url.clone() };
            let id = links::insert(&ctx, &s, &who(&a)).await?;
            let mut v = link_view(&links::get(&ctx, &id).await?);
            // The same link through the main port, for a browser that only reaches the public address.
            if acc.kind == config::AZURE {
                v["publicUrl"] = serde_json::json!(format!("{}/blob/{acct2}/{cont2}/{path2}?{q}", ctx.public_base()));
            }
            Ok(v)
        })
        .req("account", "account").req("container", "container").req("path", "path").param("allows", "read, write, delete, list (default read)").param("valid_for", "e.g. 1h (default)").param("ips", "an IP or a range a-b")
        .route("POST", "/storage/{account}/{container}/links/{*path}"),
        Tool::new("set_next_use", "How a link's next use answers: expired | bad_signature | ip_denied | permission | not_yet_valid | gone | normal. Once, then normal.", |ctx, a| async move {
            let id = a.need("id")?;
            let r = a.need("result")?;
            if !links::NEXT_USE.iter().any(|x| x.0 == r) {
                return Err(ApiError::bad(format!("result: {}", links::NEXT_USE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | "))));
            }
            let t = a.u64("times").unwrap_or(1) as i64;
            links::set(&ctx, &id, "UPDATE storage_links SET next_use=?2, next_times=?3 WHERE id=?1", if r == "normal" { None } else { Some(r) }, Some(t)).await?;
            Ok(link_view(&links::get(&ctx, &id).await?))
        })
        .req("id", "link id").req("result", "expired | bad_signature | ip_denied | permission | not_yet_valid | gone | normal").param("times", "default 1")
        .route("PUT", "/storage/links/{id}/next-use"),
        Tool::new("revoke_link", "Its next use answers 403 AuthenticationFailed, like Azure after the signing key rotates. The app is not told.", |ctx, a| async move {
            let id = a.need("id")?;
            links::set(&ctx, &id, "UPDATE storage_links SET revoked_at=?3, revoked_by=?2 WHERE id=?1", Some(who(&a)), Some(util::now_ms())).await?;
            Ok(link_view(&links::get(&ctx, &id).await?))
        })
        .req("id", "link id")
        .route("POST", "/storage/links/{id}/revoke"),
        Tool::new("extend_link", "Pushes a link's expiry (Stuntman then accepts it until the new time).", |ctx, a| async move {
            let id = a.need("id")?;
            let l = links::get(&ctx, &id).await?;
            let by_ms = a.ms("by")?.unwrap_or(3_600_000) as i64;
            let to = l["expiry"].as_i64().unwrap_or(0).max(util::now_ms()) + by_ms;
            links::set(&ctx, &id, "UPDATE storage_links SET expiry=?3 WHERE id=?1 AND ?2 IS NULL", None, Some(to)).await?;
            Ok(link_view(&links::get(&ctx, &id).await?))
        })
        .req("id", "link id").param("by", "default 1h")
        .route("POST", "/storage/links/{id}/extend"),
        Tool::new("remove_link", "Removes a link from the list (its URL keeps working as it would).", |ctx, a| async move {
            let id = a.need("id")?;
            links::set(&ctx, &id, "UPDATE storage_links SET removed=1 WHERE id=?1 AND ?2 IS NULL AND ?3 IS NULL", None, None).await?;
            Ok(json!({"removed": id}))
        })
        .req("id", "link id")
        .route("DELETE", "/storage/links/{id}"),
    ]
}

// ---------------------------------------------------------------- S4 events

async fn sub_view(ctx: &Ctx, s: &events::Sub) -> Value {
    let (validated, error, next) = events::sub_state(ctx, &s.name).await.unwrap_or_default();
    let evs = events::list(ctx, Some(s.name.clone()), None, 200).await.unwrap_or_default();
    let retrying = evs.iter().filter(|e| e["status"] == "retrying").count();
    let health = if s.to == "queue" {
        "waiting for the Queues service".to_string()
    } else if s.format == "event_grid" && !validated {
        if error.is_empty() { "validating".into() } else { "not subscribed".into() }
    } else if retrying > 0 {
        "retrying".into()
    } else {
        "delivering".into()
    };
    let mut v = json!(s);
    v["health"] = json!(health);
    v["retrying"] = json!(retrying);
    v["error"] = json!(error);
    v["nextDelivery"] = json!(if next.is_empty() { Value::Null } else { json!(next) });
    v["events"] = json!(evs.len());
    v
}

fn events_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_subscriptions", "Where storage events go: webhook or queue topic, filters, format and delivery health.", |ctx, _a| async move {
            let mut out = vec![];
            for s in events::subs(&ctx) {
                out.push(sub_view(&ctx, &s).await);
            }
            Ok(json!({"subscriptions": out}))
        })
        .route("GET", "/storage/subscriptions"),
        Tool::new("create_subscription", "A subscription: to = webhook (url) or queue (entity), events created/deleted, under (glob over container/path), format event_grid | s3. Event Grid's validation handshake runs first.", |ctx, a| async move {
            let s = events::Sub {
                name: a.str("name").unwrap_or_else(|| format!("{} {}", a.str("url").or(a.str("entity")).unwrap_or_default().split("//").last().unwrap_or("").split('/').next().unwrap_or("sub"), a.str("to").unwrap_or("webhook".into()))),
                to: a.str("to").unwrap_or("webhook".into()),
                url: a.str("url").unwrap_or_default(),
                entity: a.str("entity").unwrap_or_default(),
                events: { let e = a.list("events"); if e.is_empty() { vec!["created".into(), "deleted".into()] } else { e.into_iter().map(|x| x.to_lowercase().replace("blob", "")).collect() } },
                under: a.str("under").unwrap_or_default(),
                format: a.str("format").unwrap_or("event_grid".into()),
                account: a.str("account").unwrap_or_default(),
            };
            events::validate(&s).map_err(ApiError::bad)?;
            let mut all = events::subs(&ctx);
            if all.iter().any(|x| x.name == s.name) {
                return Err(ApiError::new(409, format!("A subscription “{}” exists", s.name)));
            }
            let hs = events::handshake(&ctx, &s).await;
            events::set_sub_state(&ctx, &s.name, Some(hs.is_ok()), Some(hs.clone().err().unwrap_or_default()), None).await?;
            all.push(s.clone());
            events::save(&ctx, all)?;
            Ok(sub_view(&ctx, &s).await)
        })
        .param("name", "a label").param("to", "webhook | queue").param("url", "webhook URL").param("entity", "queue: namespace/topic").param("events", "created, deleted").param("under", "glob, e.g. documents/**")
        .param("format", "event_grid | s3").param("account", "only this account")
        .route("POST", "/storage/subscriptions"),
        Tool::new("validate_subscription", "Runs the validation handshake again.", |ctx, a| async move {
            let n = a.need("id")?;
            let s = events::subs(&ctx).into_iter().find(|s| s.name == n).ok_or_else(|| ApiError::not_found(format!("No subscription “{n}”")))?;
            let hs = events::handshake(&ctx, &s).await;
            events::set_sub_state(&ctx, &s.name, Some(hs.is_ok()), Some(hs.err().unwrap_or_default()), None).await?;
            Ok(sub_view(&ctx, &s).await)
        })
        .req("id", "subscription name")
        .route("POST", "/storage/subscriptions/{id}/validate"),
        Tool::new("delete_subscription", "Removes a subscription; its events stay listed.", |ctx, a| async move {
            let n = a.need("id")?;
            let mut all = events::subs(&ctx);
            let before = all.len();
            all.retain(|s| s.name != n);
            if all.len() == before {
                return Err(ApiError::not_found(format!("No subscription “{n}”")));
            }
            events::save(&ctx, all)?;
            Ok(json!({"deleted": n}))
        })
        .req("id", "subscription name")
        .route("DELETE", "/storage/subscriptions/{id}"),
        Tool::new("list_events", "Storage events with where they went and the answer. result = delivered | retrying | given_up | pending | dropped | held.", |ctx, a| async move {
            let last = a.u64("last").unwrap_or(50) as i64;
            let mut evs = events::list(&ctx, a.str("subscription"), a.str("result"), last).await?;
            if let Some(q) = a.str("q") {
                evs.retain(|e| e["path"].as_str().unwrap_or("").contains(&q) || e["type"].as_str().unwrap_or("").contains(&q));
            }
            for e in evs.iter_mut() {
                if let Some(o) = e.as_object_mut() {
                    o.remove("payload");
                }
            }
            Ok(json!({"events": evs}))
        })
        .param("subscription", "name").param("result", "delivered | retrying | given_up | pending | dropped | held").param("last", "default 50").param("q", "path or type")
        .route("GET", "/storage/events"),
        Tool::new("get_event", "An event: payload in the provider's schema, delivery attempts, and the request that caused it.", |ctx, a| async move {
            let id = a.need("id")?;
            let mut e = events::get(&ctx, &id).await?;
            if let Some(seq) = e["cause"].as_i64() {
                let c = ctx.db.call(move |cn| cn.query_row("SELECT seq, at, account, container, path, op, method, status, caller, via, link_id, provoked, ms, size, ctype, remote, detail FROM storage_requests WHERE seq=?1", [seq], store::request_json)).await.ok();
                e["causedBy"] = json!(c);
            }
            Ok(e)
        })
        .req("id", "event id")
        .route("GET", "/storage/events/{id}"),
        Tool::new("send_event", "Sends an event by hand to one subscription: type created | deleted, for a path (container/path); recorded as sent by you, the object is not changed.", |ctx, a| async move {
            let sub = a.need("subscription").or(a.need("to"))?;
            let t = a.str("type").unwrap_or("created".into()).to_lowercase();
            let full = a.need("path")?;
            let (cont, path) = full.split_once('/').ok_or_else(|| ApiError::bad("path: container/path"))?;
            let acc = config::list(&ctx).into_iter().find(|x| x.containers.contains_key(cont)).ok_or_else(|| ApiError::not_found(format!("No container “{cont}”")))?;
            let b = store::get(&ctx, &acc.account, cont, path, false).await?.unwrap_or(store::Blob { account: acc.account.clone(), container: cont.into(), path: path.into(), ctype: mime(path), etag: "\"0x0\"".into(), ..Default::default() });
            let url = ops::object_url(&ctx, &acc, cont, path);
            let o = events::Obj { account: &acc.account, container: cont, path, size: b.size, ctype: &b.ctype, etag: &b.etag, url };
            let created = t.contains("creat");
            let ids = events::emit(&ctx, created, "PutBlob", &o, &format!("sent by {}", who(&a)), None, Some(&sub)).await?;
            if ids.is_empty() {
                return Err(ApiError::not_found(format!("No subscription “{sub}”")));
            }
            Ok(events::get(&ctx, &ids[0]).await?)
        })
        .req("subscription", "where to").param("type", "created | deleted").req("path", "container/path")
        .route("POST", "/storage/events"),
        Tool::new("set_next_delivery", "How the next event to a subscription is delivered: hold:30s | duplicate | reorder | drop | missing_field | normal. Once.", |ctx, a| async move {
            let n = a.need("id").or(a.need("subscription"))?;
            let b = a.need("behavior").or(a.need("behaviour"))?;
            if !events::NEXT.iter().any(|x| x.0 == b) && !b.starts_with("hold:") {
                return Err(ApiError::bad("behavior: hold:30s | duplicate | reorder | drop | missing_field | normal"));
            }
            events::subs(&ctx).into_iter().find(|s| s.name == n).ok_or_else(|| ApiError::not_found(format!("No subscription “{n}”")))?;
            events::set_sub_state(&ctx, &n, None, None, Some(if b == "normal" { String::new() } else { b.clone() })).await?;
            Ok(json!({"subscription": n, "nextDelivery": b}))
        })
        .req("id", "subscription name").req("behavior", "hold:30s | duplicate | reorder | drop | missing_field | normal")
        .route("PUT", "/storage/subscriptions/{id}/next-delivery"),
        Tool::new("resend_event", "Delivers an event again (to_all: to every subscription, as new events).", |ctx, a| async move {
            let id = a.need("id")?;
            let ids = events::resend(&ctx, &id, a.bool("to_all").unwrap_or(false), &format!("sent by {}", who(&a))).await?;
            Ok(json!({"events": ids}))
        })
        .req("id", "event id").param("to_all", "true: send to every subscription")
        .route("POST", "/storage/events/{id}/resend"),
        Tool::new("wait_for_storage_delivery", "Waits for a delivery to a subscription (optionally of a type and path) and returns it with the app's answer.", |ctx, a| async move {
            let sub = a.need("subscription")?;
            let since = a.get("since").and_then(|v| v.as_i64()).unwrap_or(util::now_ms() - 2000);
            let end = util::now_ms() + a.ms("timeout")?.unwrap_or(30_000) as i64;
            let (t, p) = (a.str("type"), a.str("path"));
            loop {
                let evs = events::list(&ctx, Some(sub.clone()), None, 100).await?;
                if let Some(e) = evs.iter().rev().find(|e| e["at"].as_i64().unwrap_or(0) >= since && e["attempts"].as_i64().unwrap_or(0) > 0
                    && t.as_ref().map(|t| e["type"].as_str().unwrap_or("").contains(t.as_str())).unwrap_or(true) && p.as_ref().map(|p| e["path"].as_str().unwrap_or("").contains(p.as_str())).unwrap_or(true)) {
                    return Ok(e.clone());
                }
                if util::now_ms() > end {
                    return Err(ApiError::new(408, format!("No delivery to {sub} yet")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .req("subscription", "name").param("type", "BlobCreated…").param("path", "part of the path").param("timeout", "default 30s").param("since", "ms; default 2 s ago")
        .route("GET", "/storage/events/wait"),
    ]
}

// ---------------------------------------------------------------- S2 rules

fn rule_from(a: &Args, base: Option<rules::Rule>) -> Result<rules::Rule, ApiError> {
    let mut r = base.unwrap_or_default();
    if let Some(n) = rule_name(a) {
        if r.name.is_empty() {
            r.name = n;
        }
    }
    if let Some(n) = a.str("new_name") {
        r.name = n;
    }
    if let Some(w) = a.get("when") {
        r.when = serde_json::from_value(w.clone()).map_err(|e| ApiError::bad(format!("when: {e}")))?;
    }
    if let Some(t) = a.get("then") {
        r.then = match t {
            Value::String(s) => rules::Then { respond: s.clone(), seconds: None },
            t => serde_json::from_value(t.clone()).map_err(|e| ApiError::bad(format!("then: {e}")))?,
        };
    }
    if let Some(h) = a.get("how_often") {
        r.how_often = match h {
            Value::String(s) if s == "every" || s == "every_time" => rules::HowOften::default(),
            h => serde_json::from_value(h.clone()).map_err(|e| ApiError::bad(format!("how_often: {e}")))?,
        };
    }
    if let Some(e) = a.bool("enabled") {
        r.enabled = e;
    }
    r.validate().map_err(ApiError::bad)?;
    Ok(r)
}

async fn rules_view(ctx: &Ctx, service: &str) -> Result<Value, ApiError> {
    let f = rules::load(ctx, service)?;
    let hits = rules::hits(ctx, service).await?;
    let accounts: Vec<String> = config::list(ctx).into_iter().filter(|a| a.kind == service).map(|a| a.account).collect();
    let last = store::last_requests(ctx, accounts, 50).await?;
    let out: Vec<Value> = f.rules.iter().map(|r| {
        let h = hits.get(&r.name).cloned().unwrap_or((0, None));
        let mut v = json!(r);
        v["meta"] = json!(r.meta());
        v["hitsToday"] = json!(h.0);
        v["lastHit"] = json!(h.1);
        v["warnings"] = rules::test(r, &f.rules, &last)["warnings"].clone();
        v
    }).collect();
    Ok(json!({"service": service, "rules": out, "file": format!("storage/rules/{service}.yaml"), "yaml": std::fs::read_to_string(rules::file_path(ctx, service)).unwrap_or_default()}))
}

fn rules_tools() -> Vec<Tool> {
    vec![
        Tool::new("list_rule_options", "The operations, “only when” fields and responses this service offers (the rule editor's lists).", |ctx, a| async move {
            let s = service(&a)?;
            let accounts: Vec<Value> = config::list(&ctx).into_iter().filter(|x| x.kind == s).map(|x| json!({"account": x.account, "containers": x.containers.keys().collect::<Vec<_>>()})).collect();
            Ok(json!({"service": s, "operations": rules::OPS.iter().map(|o| json!({"value": o.0, "label": o.1})).collect::<Vec<_>>(),
                "responses": rules::RESPONSES.iter().map(|r| json!({"group": r.0, "value": r.1, "label": r.2})).collect::<Vec<_>>(),
                "onlyWhen": [{"field": "size", "ops": ["over", "under"]}, {"field": "content_type", "ops": ["is", "contains", "is_not"]}, {"field": "caller", "ops": ["is", "is_not", "contains"]}],
                "accounts": accounts}))
        })
        .req("service", "azure-blob | s3")
        .route("GET", "/services/{name}/rules/options"),
        Tool::new("list_rules", "Rules in order (the first that matches answers): enabled or not, hits today, last hit, warnings.", |ctx, a| async move { rules_view(&ctx, &service(&a)?).await })
            .req("service", "azure-blob | s3")
            .route("GET", "/services/{name}/rules"),
        Tool::new("test_rule", "Try it: what the rule would match among the last requests and what the app would see per attempt. Nothing is saved.", |ctx, a| async move {
            let s = service(&a)?;
            let f = rules::load(&ctx, &s)?;
            let name = rule_name(&a).unwrap_or("draft".into());
            let base = f.rules.iter().find(|r| r.name == name).cloned();
            let mut r = rule_from(&a, base.clone().or(Some(rules::Rule { name: name.clone(), enabled: true, ..Default::default() })))?;
            if base.is_none() {
                r.name = name;
            }
            let accounts: Vec<String> = config::list(&ctx).into_iter().filter(|x| x.kind == s).map(|x| x.account).collect();
            let last = store::last_requests(&ctx, accounts, a.u64("last").unwrap_or(50) as i64).await?;
            let mut others = f.rules.clone();
            if !others.iter().any(|x| x.name == r.name) {
                others.push(r.clone());
            }
            Ok(rules::test(&r, &others, &last))
        })
        .req("service", "azure-blob | s3").param("rule", "rule name (HTTP; MCP: name)").param("when", "{op, account, container, path, only_when}").param("then", "{respond, seconds}").param("how_often", "{first: N} | {random: %} | every").param("last", "default 50")
        .route("POST", "/services/{name}/rules/test"),
        Tool::new("create_rule", "A rule: when (op, account, container, path glob, only_when) → then (respond, seconds) → how_often (first N, random %, every time). Warns if a rule above answers the same requests.", |ctx, a| async move {
            let s = service(&a)?;
            let mut f = rules::load(&ctx, &s)?;
            let r = rule_from(&a, Some(rules::Rule { enabled: true, ..Default::default() }))?;
            if f.rules.iter().any(|x| x.name == r.name) {
                return Err(ApiError::new(409, format!("A rule “{}” exists", r.name)));
            }
            f.rules.push(r.clone());
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &r.name).await?;
            let v = rules_view(&ctx, &s).await?;
            Ok(v["rules"].as_array().and_then(|x| x.iter().find(|y| y["name"] == json!(r.name)).cloned()).unwrap_or(Value::Null))
        })
        .req("service", "azure-blob | s3").req("name", "rule name (HTTP body: rule)").param("when", "{op, account, container, path, only_when}").req("then", "{respond, seconds}").param("how_often", "{first: N} | {random: %} | every").param("enabled", "default true")
        .route("POST", "/services/{name}/rules"),
        Tool::new("update_rule", "Applies a patch to a rule (when, then, how_often, enabled, new_name). The “first N” count starts again.", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("“rule” (name) is required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let i = f.rules.iter().position(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            let r = rule_from(&a, Some(f.rules[i].clone()))?;
            f.rules[i] = r.clone();
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &r.name).await?;
            let v = rules_view(&ctx, &s).await?;
            Ok(v["rules"].as_array().and_then(|x| x.iter().find(|y| y["name"] == json!(r.name)).cloned()).unwrap_or(Value::Null))
        })
        .req("service", "azure-blob | s3").req("name", "rule name (HTTP: in the path)").param("when", "patch").param("then", "patch").param("how_often", "patch").param("enabled", "true | false").param("new_name", "rename")
        .route("PATCH", "/services/{name}/rules/{rule}"),
        Tool::new("set_rule_enabled", "Switches a rule on or off; the “how often” count starts again.", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let r = f.rules.iter_mut().find(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            r.enabled = a.bool("enabled").unwrap_or(true);
            rules::save(&ctx, &s, &f)?;
            rules::reset_count(&ctx, &s, &n).await?;
            Ok(json!({"rule": n, "enabled": a.bool("enabled").unwrap_or(true)}))
        })
        .req("service", "azure-blob | s3").req("name", "rule name").req("enabled", "true | false")
        .route("PUT", "/services/{name}/rules/{rule}/enabled"),
        Tool::new("move_rule", "Moves a rule to a position (1 is first); order matters.", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let i = f.rules.iter().position(|r| r.name == n).ok_or_else(|| ApiError::not_found(format!("No rule “{n}”")))?;
            let r = f.rules.remove(i);
            let p = (a.u64("position").unwrap_or(1).max(1) as usize - 1).min(f.rules.len());
            f.rules.insert(p, r);
            rules::save(&ctx, &s, &f)?;
            Ok(json!({"order": f.rules.iter().map(|r| r.name.clone()).collect::<Vec<_>>()}))
        })
        .req("service", "azure-blob | s3").req("name", "rule name").req("position", "1 is first")
        .route("PUT", "/services/{name}/rules/{rule}/position"),
        Tool::new("delete_rule", "Deletes a rule.", |ctx, a| async move {
            let s = service(&a)?;
            let n = rule_name(&a).ok_or_else(|| ApiError::bad("rule name required"))?;
            let mut f = rules::load(&ctx, &s)?;
            let before = f.rules.len();
            f.rules.retain(|r| r.name != n);
            if f.rules.len() == before {
                return Err(ApiError::not_found(format!("No rule “{n}”")));
            }
            rules::save(&ctx, &s, &f)?;
            Ok(json!({"deleted": n}))
        })
        .req("service", "azure-blob | s3").req("name", "rule name")
        .route("DELETE", "/services/{name}/rules/{rule}"),
        Tool::new("put_rules_yaml", "Replaces the whole rules file.", |ctx, a| async move {
            let s = service(&a)?;
            let f: rules::File = serde_yaml::from_str(&a.need("yaml")?).map_err(|e| ApiError::bad(format!("YAML: {e}")))?;
            rules::save(&ctx, &s, &f)?;
            rules_view(&ctx, &s).await
        })
        .req("service", "azure-blob | s3").req("yaml", "the whole file")
        .route("PUT", "/services/{name}/rules.yaml"),
    ]
}

// ---------------------------------------------------------------- S5 accounts

fn kind_ok(a: &Account, s: &str) -> Result<(), ApiError> {
    if a.kind != s {
        return Err(ApiError::not_found(format!("{} is a {} account", a.account, a.kind)));
    }
    Ok(())
}

/// Applies a JSON patch shaped like the YAML (access, containers, behaviour, seed).
fn patch(a: &mut Account, p: &Value) -> Result<(), ApiError> {
    let mut cur = serde_json::to_value(&*a).map_err(|e| ApiError::internal(e.to_string()))?;
    fn merge(t: &mut Value, p: &Value) {
        match (t, p) {
            (Value::Object(t), Value::Object(p)) => {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge(t.entry(k.clone()).or_insert(Value::Null), v);
                    }
                }
            }
            (t, p) => *t = p.clone(),
        }
    }
    merge(&mut cur, p);
    *a = serde_json::from_value(cur).map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(())
}

fn accounts() -> Vec<Tool> {
    vec![
        Tool::new("get_service_schema", "The fields of a storage service's account form: types, limits, help (the form is drawn from it).", |_ctx, a| async move { Ok(config::schema(&service(&a)?)) })
            .req("service", "azure-blob | s3")
            .route("GET", "/services/{name}/schema"),
        Tool::new("list_service_accounts", "Accounts of one storage service with endpoint, containers and counts.", |ctx, a| async move {
            let s = service(&a)?;
            let mut out = vec![];
            for x in config::list(&ctx).into_iter().filter(|x| x.kind == s) {
                out.push(account_view(&ctx, &x).await);
            }
            Ok(json!({"service": s, "accounts": out}))
        })
        .req("service", "azure-blob | s3")
        .route("GET", "/services/{name}/accounts"),
        Tool::new("get_account", "The settings as YAML and JSON, the connection string, keys (masked) and the file's etag for If-Match.", |ctx, a| async move {
            let s = service(&a)?;
            let acc = config::get(&ctx, &a.need("account")?)?;
            kind_ok(&acc, &s)?;
            Ok(account_view(&ctx, &acc).await)
        })
        .req("service", "azure-blob | s3").req("account", "account")
        .route("GET", "/services/{name}/accounts/{account}"),
        Tool::new("create_account", "A new account with its keys (Azure key1/key2, S3 one access key). settings: containers {name: {access}}, behaviour, access.", |ctx, a| async move {
            let s = service(&a)?;
            let name = a.need("account")?;
            config::valid_account(&name).map_err(ApiError::bad)?;
            let containers: Vec<(String, String)> = match a.get("containers") {
                Some(Value::Array(v)) => v.iter().map(|x| (x.as_str().unwrap_or("").to_string(), String::new())).collect(),
                Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v["access"].as_str().unwrap_or("").to_string())).collect(),
                Some(Value::String(x)) => x.split(',').map(|c| (c.trim().to_string(), String::new())).filter(|c| !c.0.is_empty()).collect(),
                _ => vec![],
            };
            let mut acc = config::create(&ctx, &s, &name, &containers)?;
            if let Some(p) = a.get("settings") {
                patch(&mut acc, p)?;
                config::save(&ctx, &acc)?;
            }
            Ok(account_view(&ctx, &acc).await)
        })
        .req("service", "azure-blob | s3").req("account", "3 to 24 lowercase letters, numbers or dashes").param("containers", "list or {name: {access}}").param("settings", "same keys as the YAML")
        .route("POST", "/services/{name}/accounts"),
        Tool::new("update_account", "Applies a patch (same keys as the YAML); refused with 412 if the file changed since the etag you read (if_match).", |ctx, a| async move {
            let s = service(&a)?;
            let name = a.need("account")?;
            let mut acc = config::get(&ctx, &name)?;
            kind_ok(&acc, &s)?;
            let (text, etag) = config::text(&ctx, &name);
            if let Some(m) = a.str("if_match") {
                if m != etag {
                    return Err(ApiError::new(412, format!("{} changed since you read it", config::rel_file(&name))).with(json!({"yaml": text, "etag": etag})));
                }
            }
            patch(&mut acc, a.get("patch").or(a.get("settings")).ok_or_else(|| ApiError::bad("“patch” is required"))?)?;
            if acc.account != name {
                return Err(ApiError::bad("Renaming an account: make a new one (the name is in every URL and key)"));
            }
            config::save(&ctx, &acc)?;
            Ok(account_view(&ctx, &acc).await)
        })
        .req("service", "azure-blob | s3").req("account", "account").req("patch", "same keys as the YAML").param("if_match", "the etag you read")
        .route("PATCH", "/services/{name}/accounts/{account}"),
        Tool::new("rotate_key", "Makes a new value for a key: apps signing with the old one now get 403. The new value is returned once.", |ctx, a| async move {
            let s = service(&a)?;
            let acc = config::get(&ctx, &a.need("account")?)?;
            kind_ok(&acc, &s)?;
            let k = a.need("key")?;
            if !acc.access.keys.contains(&k) {
                return Err(ApiError::not_found(format!("No key “{k}” in {}", acc.account)));
            }
            let v = config::new_key(&ctx, &acc, &k)?;
            ctx.live.send("storage.account", &acc.account, "updated");
            Ok(json!({"key": k, "value": v, "masked": config::mask(&v), "connection": ops::connection(&ctx, &acc)}))
        })
        .req("service", "azure-blob | s3").req("account", "account").req("key", "key1, key2, or the S3 access key id")
        .route("POST", "/services/{name}/accounts/{account}/keys/{key}/rotate"),
        Tool::new("add_container", "Adds a container (bucket) to an account.", |ctx, a| async move {
            let s = service(&a)?;
            let mut acc = config::get(&ctx, &a.need("account")?)?;
            kind_ok(&acc, &s)?;
            let n = a.need("container").or(a.need("container_name"))?;
            if acc.containers.contains_key(&n) {
                return Err(ApiError::new(409, format!("{n} exists")));
            }
            acc.containers.insert(n.clone(), config::Container { access: a.str("access").unwrap_or("private".into()) });
            config::save(&ctx, &acc)?;
            Ok(json!({"container": n, "access": acc.containers[&n].access}))
        })
        .req("service", "azure-blob | s3").req("account", "account").req("container", "name").param("access", "private | blob | container | public-read")
        .route("POST", "/services/{name}/accounts/{account}/containers"),
        Tool::new("remove_container", "Removes a container and its objects; says how many went with it. The seed is not changed, so a reset brings them back.", |ctx, a| async move {
            let s = service(&a)?;
            let mut acc = config::get(&ctx, &a.need("account")?)?;
            kind_ok(&acc, &s)?;
            let n = a.need("container")?;
            if acc.containers.remove(&n).is_none() {
                return Err(ApiError::not_found(format!("No container “{n}”")));
            }
            config::save(&ctx, &acc)?;
            let gone = store::drop_container(&ctx, &acc.account, &n).await?;
            Ok(json!({"removed": n, "objects": gone}))
        })
        .req("service", "azure-blob | s3").req("account", "account").req("container", "name")
        .route("DELETE", "/services/{name}/accounts/{account}/containers/{container}"),
        Tool::new("delete_account", "Deletes an account, its objects and links (people confirm on the screen; agents do not).", |ctx, a| async move {
            let s = service(&a)?;
            let acc = config::get(&ctx, &a.need("account")?)?;
            kind_ok(&acc, &s)?;
            std::fs::remove_file(config::file(&ctx, &acc.account)).map_err(|e| ApiError::internal(e.to_string()))?;
            store::drop_account(&ctx, &acc.account).await?;
            ctx.live.send("storage.account", &acc.account, "deleted");
            Ok(json!({"deleted": acc.account}))
        })
        .req("service", "azure-blob | s3").req("account", "account")
        .route("DELETE", "/services/{name}/accounts/{account}"),
    ]
}
