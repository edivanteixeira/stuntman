//! The file-server tools (A15).

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use stuntman_core::{ApiError, Tool};

use crate::{config, store};

fn tree(root: &std::path::Path, base: &std::path::Path, out: &mut Vec<Value>) {
    for e in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let p = e.path();
        let rel = format!("/{}", p.strip_prefix(base).unwrap_or(&p).to_string_lossy());
        let Ok(m) = e.metadata() else { continue };
        let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
        out.push(json!({"path": rel, "dir": m.is_dir(), "size": m.len(), "modified": modified}));
        if m.is_dir() {
            tree(&p, base, out);
        }
    }
}

fn server_of(ctx: &stuntman_core::Ctx, name: &str) -> Result<config::Server, ApiError> {
    config::load(ctx).servers.into_iter().find(|s| s.name == name).ok_or_else(|| ApiError::not_found(format!("No file server “{name}” (set_file_server)")))
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_file_servers", "The FTP/FTPS/SFTP servers of this environment: users, provocation, how to connect, file counts.", |ctx, _a| async move {
            let ports = (std::env::var("STUNTMAN_FTP_PORT").unwrap_or("2121".into()), std::env::var("STUNTMAN_SFTP_PORT").unwrap_or("2222".into()));
            let list: Vec<Value> = config::load(&ctx).servers.into_iter().map(|s| {
                let mut files = vec![];
                tree(&config::root(&ctx, &s.name), &config::root(&ctx, &s.name), &mut files);
                json!({"name": s.name, "title": s.title, "users": s.users.iter().map(|u| json!({"user": u.user, "readOnly": u.read_only, "password": if u.password.is_empty() { "any (not “wrong”)" } else { "set" }})).collect::<Vec<_>>(),
                    "provoke": s.provoke, "files": files.iter().filter(|f| f["dir"] == false).count(), "folders": files.iter().filter(|f| f["dir"] == true).count(),
                    "connect": {"ftp": format!("ftp://localhost:{}", ports.0), "ftps": format!("ftp://localhost:{} with AUTH TLS (explicit)", ports.0), "sftp": format!("sftp -P {} <user>@localhost", ports.1)}})
            }).collect();
            Ok(json!({"servers": list, "provocations": config::PROVOKE.iter().map(|p| json!({"kind": p.0, "label": p.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/files/servers"),
        Tool::new("set_file_server", "Creates or changes a file server: users [{user, password, read_only}], folders to make (/in, /out), and a provocation for the next sessions: login_refused, disk_full, permission_denied, drop_mid, slow, host_key_changed (none clears).", |ctx, a| async move {
            let name = a.need("name")?;
            if !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.') {
                return Err(ApiError::bad("name: lowercase letters, digits, dots and dashes"));
            }
            let mut all = config::load(&ctx);
            let i = match all.servers.iter().position(|s| s.name == name) { Some(i) => i, None => { all.servers.push(config::Server { name: name.clone(), ..Default::default() }); all.servers.len() - 1 } };
            let s = &mut all.servers[i];
            if let Some(t) = a.str("title") { s.title = t; }
            if let Some(u) = a.get("users") { s.users = serde_json::from_value(u.clone()).map_err(|e| ApiError::bad(format!("users: {e}")))?; }
            if let Some(f) = a.get("folders") { s.folders = serde_json::from_value(f.clone()).map_err(|e| ApiError::bad(format!("folders: {e}")))?; }
            if let Some(p) = a.str("provoke") {
                if p != "none" && !config::PROVOKE.iter().any(|x| x.0 == p) {
                    return Err(ApiError::bad(format!("provoke: {} | none", config::PROVOKE.iter().map(|x| x.0).collect::<Vec<_>>().join(" | "))));
                }
                s.provoke = if p == "none" { String::new() } else { p };
            }
            if s.users.is_empty() {
                s.users.push(config::User { user: "edi".into(), ..Default::default() });
            }
            let s = s.clone();
            for f in &s.folders {
                let _ = std::fs::create_dir_all(config::disk(&ctx, &s.name, &config::resolve("/", f)));
            }
            let _ = std::fs::create_dir_all(config::root(&ctx, &s.name));
            config::save(&ctx, &all)?;
            ctx.live.send("files.servers", &name, "updated");
            Ok(json!({"server": s}))
        })
        .req("name", "e.g. edi-acme").param("title", "shown name").param("users", "[{user, password, read_only}]").param("folders", "[/in/notfis, /out/ocoren]").param("provoke", "login_refused | disk_full | permission_denied | drop_mid | slow | host_key_changed | none")
        .route("PUT", "/files/servers/{name}"),
        Tool::new("list_files", "The files and folders on a server (or under a path).", |ctx, a| async move {
            let s = server_of(&ctx, &a.need("server")?)?;
            let path = config::resolve("/", &a.str("path").unwrap_or("/".into()));
            let mut out = vec![];
            tree(&config::disk(&ctx, &s.name, &path), &config::root(&ctx, &s.name), &mut out);
            out.sort_by(|x, y| x["path"].as_str().cmp(&y["path"].as_str()));
            Ok(json!({"server": s.name, "path": path, "files": out}))
        })
        .req("server", "the server").param("path", "a folder")
        .route("GET", "/files/servers/{server}/files"),
        Tool::new("get_file", "A file's content (text, or base64 for binary).", |ctx, a| async move {
            let s = server_of(&ctx, &a.need("server")?)?;
            let path = config::resolve("/", &a.need("path")?);
            let b = std::fs::read(config::disk(&ctx, &s.name, &path)).map_err(|_| ApiError::not_found("No such file"))?;
            Ok(match String::from_utf8(b.clone()) { Ok(t) => json!({"path": path, "size": b.len(), "text": t}), Err(_) => json!({"path": path, "size": b.len(), "base64": B64.encode(&b)}) })
        })
        .req("server", "the server").req("path", "the file")
        .route("GET", "/files/servers/{server}/file"),
        Tool::new("put_file", "Puts a file on a server as if the partner dropped it (text, or base64 for binary). The app finds it on its next poll.", |ctx, a| async move {
            let s = server_of(&ctx, &a.need("server")?)?;
            let path = config::resolve("/", &a.need("path")?);
            let bytes = match (a.str("text"), a.str("base64")) { (Some(t), _) => t.into_bytes(), (None, Some(b)) => B64.decode(b).map_err(|_| ApiError::bad("base64"))?, _ => return Err(ApiError::bad("text or base64")) };
            let d = config::disk(&ctx, &s.name, &path);
            if let Some(p) = d.parent() { std::fs::create_dir_all(p).map_err(|e| ApiError::internal(e.to_string()))?; }
            std::fs::write(&d, &bytes).map_err(|e| ApiError::internal(e.to_string()))?;
            store::log(&ctx, &s.name, "you", "Stuntman", "put", &path, bytes.len() as u64, true, "put by you (as the partner)").await;
            Ok(json!({"server": s.name, "path": path, "size": bytes.len()}))
        })
        .req("server", "the server").req("path", "/in/notfis/NOTFIS_1042.txt").param("text", "the content").param("base64", "binary content")
        .route("POST", "/files/servers/{server}/put"),
        Tool::new("delete_file", "Deletes a file from a server.", |ctx, a| async move {
            let s = server_of(&ctx, &a.need("server")?)?;
            let path = config::resolve("/", &a.need("path")?);
            std::fs::remove_file(config::disk(&ctx, &s.name, &path)).map_err(|_| ApiError::not_found("No such file"))?;
            store::log(&ctx, &s.name, "you", "Stuntman", "delete", &path, 0, true, "deleted by you").await;
            Ok(json!({"deleted": path}))
        })
        .req("server", "the server").req("path", "the file")
        .route("DELETE", "/files/servers/{server}/file"),
        Tool::new("list_transfers", "Logins and transfers on the file servers, newest first.", |ctx, a| async move {
            Ok(json!({"transfers": store::list(&ctx, a.str("server"), a.u64("limit").unwrap_or(200) as i64).await?}))
        })
        .param("server", "one server").param("limit", "200")
        .route("GET", "/files/transfers"),
        Tool::new("wait_for_file", "Waits until a file whose path contains a text (or matches a * pattern) is on a server: what the app uploaded.", |ctx, a| async move {
            let s = server_of(&ctx, &a.need("server")?)?;
            let pat = a.need("path")?;
            let end = std::time::Instant::now() + std::time::Duration::from_millis(a.ms("timeout")?.unwrap_or(30_000));
            let re = regex_lite(&pat);
            loop {
                let mut out = vec![];
                tree(&config::root(&ctx, &s.name), &config::root(&ctx, &s.name), &mut out);
                if let Some(f) = out.into_iter().find(|f| f["dir"] == false && re(f["path"].as_str().unwrap_or(""))) {
                    return Ok(f);
                }
                if std::time::Instant::now() > end {
                    return Err(ApiError::new(408, format!("No file like {pat} on {} yet", s.name)));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .req("server", "the server").req("path", "/out/ocoren/*.txt or a part of the path").param("timeout", "30s")
        .route("GET", "/files/servers/{server}/wait"),
    ]
}

/// "*" globs, else "contains".
fn regex_lite(p: &str) -> impl Fn(&str) -> bool {
    let parts: Vec<String> = p.split('*').map(String::from).collect();
    let glob = p.contains('*');
    let p = p.to_string();
    move |s: &str| {
        if !glob {
            return s.contains(&p);
        }
        let mut rest = s;
        for (i, part) in parts.iter().enumerate() {
            if i == 0 {
                if !rest.starts_with(part.as_str()) { return false; }
                rest = &rest[part.len()..];
            } else if i == parts.len() - 1 {
                return rest.ends_with(part.as_str());
            } else if let Some(k) = rest.find(part.as_str()) {
                rest = &rest[k + part.len()..];
            } else {
                return false;
            }
        }
        true
    }
}
