//! What both protocols and the screen do to objects: write, delete, record, emit events. A file
//! added on the screen or over MCP goes through here too, recorded as added by that person.

use std::collections::BTreeMap;

use stuntman_core::{ApiError, Ctx};

use crate::{
    config::{self, Account},
    events,
    store::{self, Blob, Put},
};

/// Who is calling: the X-Stuntman-App header, else the first User-Agent product, "browser" for one.
/// The W3C trace id the app sent (traceparent: 00-<trace>-<span>-<flags>), so Timeline rows join a journey.
pub fn trace(h: &BTreeMap<String, String>) -> Option<String> {
    h.get("traceparent").and_then(|t| t.split('-').nth(1)).filter(|t| t.len() == 32).map(str::to_lowercase)
}

pub fn caller(h: &BTreeMap<String, String>) -> String {
    if let Some(a) = h.get("x-stuntman-app").or(h.get("x-duble-app")).filter(|s| !s.is_empty()) {
        return a.clone();
    }
    let ua = h.get("user-agent").cloned().unwrap_or_default();
    if ua.starts_with("Mozilla") {
        return "browser".into();
    }
    let first = ua.split([' ', '/']).next().unwrap_or("").to_string();
    if first.is_empty() { "app".into() } else { first }
}

/// The blob's URL on the provider port (what an event carries, what Copy URL gives).
pub fn object_url(ctx: &Ctx, a: &Account, container: &str, path: &str) -> String {
    format!("{}/{container}/{}", endpoint(ctx, a), crate::auth::aws_encode(path, true))
}

/// Azure SDKs take the account from the path only when the host is an IP (Azurite style), so the
/// default environment answers on http://127.0.0.1:10000/<account>; another environment uses the
/// real-host style http://<account>.blob.<its base domain>:10000 (its hosts entries list it), or
/// http://<env>.localhost:10000/<account> when its base domain is under .localhost (no hosts entry needed).
pub fn endpoint(ctx: &Ctx, a: &Account) -> String {
    let host = ctx.host.split(':').next().unwrap_or("localhost").to_string();
    let ip = if host == "localhost" { "127.0.0.1".to_string() } else { host.clone() };
    if a.kind == config::S3 {
        return format!("http://{ip}:{}{}", crate::s3_port(), ctx.prefix());
    }
    if ctx.prefix().is_empty() && ip.parse::<std::net::IpAddr>().is_ok() {
        format!("http://{ip}:{}/{}", crate::blob_port(), a.account)
    } else {
        let base = ctx.state.file.read().unwrap().base_domain.clone();
        // *.localhost resolves to loopback with no hosts entry, and Azure SDKs then read the account
        // from the path (as for an IP): http://<env>.localhost:10000/<account>.
        if base == "localhost" || base.ends_with(".localhost") {
            format!("http://{base}:{}/{}", crate::blob_port(), a.account)
        } else {
            // On a port the SDKs treat as an emulator's (10000 and friends), .NET and JS read the
            // account from the first path segment even with a named host: without /<account> there
            // they took the container for the account and dropped the blob name.
            let port = crate::blob_port();
            let tail = if path_style_port(port) { format!("/{}", a.account) } else { String::new() };
            format!("http://{}.blob.{base}:{port}{tail}", a.account)
        }
    }
}

/// The ports Azure SDKs (.NET, JS) take as path style (<host>/<account>/...) whatever the host.
pub fn path_style_port(p: u16) -> bool {
    matches!(p, 10000..=10004 | 10100..=10104 | 11000..=11004 | 11100..=11104)
}

/// Azure connection string, or the S3 settings an app needs.
pub fn connection(ctx: &Ctx, a: &Account) -> String {
    let key = a.access.keys.first().map(|k| config::key_value(ctx, &a.account, k)).unwrap_or_default();
    if a.kind == config::S3 {
        format!("ServiceUrl={};AccessKey={};SecretKey={};Region=auto;ForcePathStyle=true", endpoint(ctx, a), a.access.keys.first().cloned().unwrap_or_default(), key)
    } else {
        format!("DefaultEndpointsProtocol=http;AccountName={};AccountKey={};BlobEndpoint={};", a.account, key, endpoint(ctx, a))
    }
}

pub struct Write {
    pub container: String,
    pub path: String,
    pub data: Vec<u8>,
    pub ctype: String,
    pub meta: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub by: String,
    pub via: String,
    pub etag: Option<String>,
    pub truncated: bool,
}

pub async fn write(ctx: &Ctx, a: &Account, w: Write) -> Result<Blob, ApiError> {
    store::put(
        ctx,
        Put {
            account: a.account.clone(),
            container: w.container,
            path: w.path,
            ctype: if w.ctype.is_empty() { "application/octet-stream".into() } else { w.ctype },
            data: w.data,
            meta: w.meta,
            headers: w.headers,
            by: w.by,
            via: w.via,
            kind: a.kind.clone(),
            versions: a.behaviour.versions,
            truncated: w.truncated,
            etag: w.etag,
        },
    )
    .await
    .map(|b| {
        ctx.live.send("storage.object", &format!("{}/{}/{}", b.account, b.container, b.path), "created");
        b
    })
}

pub async fn delete(ctx: &Ctx, a: &Account, container: &str, path: &str) -> Result<Option<Blob>, ApiError> {
    let b = store::get(ctx, &a.account, container, path, false).await?;
    if b.is_some() {
        store::delete(ctx, &a.account, container, path, a.behaviour.soft_delete != "off").await?;
        ctx.live.send("storage.object", &format!("{}/{container}/{path}", a.account), "deleted");
    }
    Ok(b)
}

/// After a write or delete was recorded (request `seq`): the events it causes.
pub async fn emit(ctx: &Ctx, a: &Account, b: &Blob, created: bool, api: &str, by: &str, seq: Option<i64>) {
    let url = object_url(ctx, a, &b.container, &b.path);
    let o = events::Obj { account: &a.account, container: &b.container, path: &b.path, size: b.size, ctype: &b.ctype, etag: &b.etag, url };
    if let Err(e) = events::emit(ctx, created, api, &o, by, seq, None).await {
        eprintln!("storage events: {}", e.message);
    }
}

/// Which account and container, by name, or a 404 the screen can show.
pub fn container_of(ctx: &Ctx, account: &str, container: &str) -> Result<Account, ApiError> {
    let a = config::get(ctx, account)?;
    if !a.containers.contains_key(container) {
        return Err(ApiError::not_found(format!("No container “{container}” in {account}")));
    }
    Ok(a)
}
