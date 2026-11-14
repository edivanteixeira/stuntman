//! S4 · Events: BlobCreated/BlobDeleted (Event Grid schema) or ObjectCreated/ObjectRemoved (S3
//! notification schema), delivered to webhooks with the provider's retry policy. Subscriptions
//! live in storage/subscriptions.yaml; Event Grid's validation handshake runs first.

use stuntman_core::{util, yamlfile, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{config, rules::glob};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sub {
    pub name: String,
    /// webhook | queue
    #[serde(default = "webhook")]
    pub to: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// queue: "namespace/topic" (delivered once the Queues service exists).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub entity: String,
    /// created, deleted
    #[serde(default = "both")]
    pub events: Vec<String>,
    /// Glob over "container/path"; empty is everything.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub under: String,
    /// event_grid | s3
    #[serde(default = "event_grid")]
    pub format: String,
    /// Only this account; empty is every account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
}
fn webhook() -> String {
    "webhook".into()
}
fn both() -> Vec<String> {
    vec!["created".into(), "deleted".into()]
}
fn event_grid() -> String {
    "event_grid".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct File {
    #[serde(default)]
    pub subscriptions: Vec<Sub>,
}

pub fn file(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("storage/subscriptions.yaml")
}

pub fn subs(ctx: &Ctx) -> Vec<Sub> {
    yamlfile::load::<File>(&file(ctx)).unwrap_or_default().subscriptions
}

/// Event Grid's retry schedule: 10 s, 30 s, 1 min, 5 min, 10 min, 30 min, 1 h, then hourly; 30
/// attempts or 24 h.
pub fn backoff_ms(attempt: i64) -> i64 {
    match attempt {
        1 => 10_000,
        2 => 30_000,
        3 => 60_000,
        4 => 300_000,
        5 => 600_000,
        6 => 1_800_000,
        _ => 3_600_000,
    }
}

pub const NEXT: [(&str, &str); 6] = [
    ("normal", "Normal"),
    ("hold:30s", "Hold 30 s, then deliver"),
    ("duplicate", "Deliver twice"),
    ("reorder", "Deliver after the next one"),
    ("drop", "Drop it"),
    ("missing_field", "Send it with a field missing"),
];

fn type_name(format: &str, created: bool, api: &str) -> String {
    match (format, created) {
        ("s3", true) => format!("ObjectCreated:{}", if api == "CopyBlob" { "Copy" } else if api == "PutBlockList" { "CompleteMultipartUpload" } else { "Put" }),
        ("s3", false) => "ObjectRemoved:Delete".into(),
        (_, true) => "Microsoft.Storage.BlobCreated".into(),
        _ => "Microsoft.Storage.BlobDeleted".into(),
    }
}

/// "BlobCreated" as the list shows it.
pub fn short(t: &str) -> String {
    t.rsplit('.').next().unwrap_or(t).to_string()
}

pub struct Obj<'a> {
    pub account: &'a str,
    pub container: &'a str,
    pub path: &'a str,
    pub size: i64,
    pub ctype: &'a str,
    pub etag: &'a str,
    pub url: String,
}

pub fn payload(format: &str, created: bool, api: &str, o: &Obj, id: &str) -> Value {
    let t = type_name(format, created, api);
    let now = crate::auth::ms_iso(util::now_ms());
    if format == "s3" {
        return json!({"Records": [{"eventVersion": "2.1", "eventSource": "aws:s3", "awsRegion": "auto", "eventTime": now, "eventName": t,
            "userIdentity": {"principalId": "stuntman"}, "requestParameters": {"sourceIPAddress": "127.0.0.1"},
            "responseElements": {"x-amz-request-id": id}, "s3": {"s3SchemaVersion": "1.0", "configurationId": "stuntman",
            "bucket": {"name": o.container, "ownerIdentity": {"principalId": "stuntman"}, "arn": format!("arn:aws:s3:::{}", o.container)},
            "object": {"key": o.path, "size": o.size, "eTag": o.etag.trim_matches('"'), "sequencer": format!("{:016X}", util::now_ms())}}}]});
    }
    json!([{
        "topic": format!("/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/stuntman/providers/Microsoft.Storage/storageAccounts/{}", o.account),
        "subject": format!("/blobServices/default/containers/{}/blobs/{}", o.container, o.path),
        "eventType": t, "id": id, "eventTime": now, "dataVersion": "", "metadataVersion": "1",
        "data": {"api": api, "clientRequestId": id, "requestId": id, "eTag": o.etag.trim_matches('"'), "contentType": o.ctype,
                 "contentLength": o.size, "blobType": "BlockBlob", "url": o.url, "sequencer": format!("{:032X}", util::now_ms()),
                 "storageDiagnostics": {"batchId": id}}
    }])
}

fn remove_field(format: &str, p: &mut Value) {
    if format == "s3" {
        if let Some(o) = p.pointer_mut("/Records/0/s3/object").and_then(|o| o.as_object_mut()) {
            o.remove("size");
        }
    } else if let Some(o) = p.pointer_mut("/0/data").and_then(|o| o.as_object_mut()) {
        o.remove("contentLength");
    }
}

/// Records an event for every subscription that wants it; `by` is the app or a person.
pub async fn emit(ctx: &Ctx, created: bool, api: &str, o: &Obj<'_>, by: &str, cause: Option<i64>, only: Option<&str>) -> Result<Vec<String>, ApiError> {
    let mut ids = vec![];
    for s in subs(ctx) {
        if only.map(|n| n != s.name).unwrap_or(false) {
            continue;
        }
        if only.is_none() {
            let want = if created { "created" } else { "deleted" };
            if !s.events.iter().any(|e| e == want) || (!s.account.is_empty() && s.account != o.account) || !glob(&s.under, &format!("{}/{}", o.container, o.path)) {
                continue;
            }
        }
        let id = util::uuid();
        let p = payload(&s.format, created, api, o, &id);
        let t = type_name(&s.format, created, api);
        let (sn, i, a, c, pa, b) = (s.name.clone(), id.clone(), o.account.to_string(), o.container.to_string(), o.path.to_string(), by.to_string());
        let status = if s.to == "queue" { "no_destination" } else { "pending" };
        ctx.db
            .call(move |cn| {
                cn.execute(
                    "INSERT INTO storage_events(id, sub, type, account, container, path, payload, at, by_who, cause, status, next_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?8)",
                    params![i, sn, t, a, c, pa, p.to_string(), util::now_ms(), b, cause, status],
                )
                .map(|_| ())
            })
            .await?;
        ids.push(id);
    }
    if !ids.is_empty() {
        ctx.live.send("storage.event", &ids[0], "created");
    }
    Ok(ids)
}

const COLS: &str = "id, sub, type, account, container, path, payload, at, by_who, cause, status, attempts, next_at, last_status, last_ms, history, behaviour";

fn ev(r: &Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, String>(0)?, "subscription": r.get::<_, String>(1)?, "type": r.get::<_, String>(2)?, "short": short(&r.get::<_, String>(2)?),
        "account": r.get::<_, String>(3)?, "container": r.get::<_, String>(4)?, "path": r.get::<_, String>(5)?,
        "payload": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null), "at": r.get::<_, i64>(7)?, "by": r.get::<_, String>(8)?,
        "cause": r.get::<_, Option<i64>>(9)?, "status": r.get::<_, String>(10)?, "attempts": r.get::<_, i64>(11)?, "nextAt": r.get::<_, Option<i64>>(12)?,
        "lastStatus": r.get::<_, Option<i64>>(13)?, "lastMs": r.get::<_, Option<i64>>(14)?,
        "history": serde_json::from_str::<Value>(&r.get::<_, String>(15)?).unwrap_or(json!([])), "behaviour": r.get::<_, String>(16)?}))
}

pub async fn list(ctx: &Ctx, sub: Option<String>, status: Option<String>, last: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut s = cn.prepare(&format!("SELECT {COLS} FROM storage_events WHERE (?1 IS NULL OR sub=?1) AND (?2 IS NULL OR status=?2) ORDER BY at DESC LIMIT ?3"))?;
            let v = s.query_map(params![sub, status, last], ev)?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let i = id.to_string();
    ctx.db.call(move |cn| cn.query_row(&format!("SELECT {COLS} FROM storage_events WHERE id=?1"), [i], ev).optional()).await?.ok_or_else(|| ApiError::not_found(format!("No event {id}")))
}

/// Subscription state kept in SQLite: validated, last error, armed next delivery.
pub async fn sub_state(ctx: &Ctx, name: &str) -> Result<(bool, String, String), ApiError> {
    let n = name.to_string();
    Ok(ctx
        .db
        .call(move |cn| cn.query_row("SELECT validated, error, next_delivery FROM storage_subs WHERE name=?1", [n], |r| Ok((r.get::<_, i64>(0)? != 0, r.get(1)?, r.get(2)?))).optional())
        .await?
        .unwrap_or((false, String::new(), String::new())))
}

pub async fn set_sub_state(ctx: &Ctx, name: &str, validated: Option<bool>, error: Option<String>, next: Option<String>) -> Result<(), ApiError> {
    let n = name.to_string();
    ctx.db
        .call(move |cn| {
            cn.execute("INSERT OR IGNORE INTO storage_subs(name, at) VALUES(?1, ?2)", params![n, util::now_ms()])?;
            if let Some(v) = validated {
                cn.execute("UPDATE storage_subs SET validated=?2 WHERE name=?1", params![n, v as i64])?;
            }
            if let Some(e) = error {
                cn.execute("UPDATE storage_subs SET error=?2 WHERE name=?1", params![n, e])?;
            }
            if let Some(x) = next {
                cn.execute("UPDATE storage_subs SET next_delivery=?2 WHERE name=?1", params![n, x])?;
            }
            Ok(())
        })
        .await?;
    ctx.live.send("storage.subscription", name, "updated");
    Ok(())
}

/// Event Grid's handshake: a SubscriptionValidationEvent the endpoint must answer with its code.
pub async fn handshake(ctx: &Ctx, s: &Sub) -> Result<(), String> {
    if s.to != "webhook" || s.format != "event_grid" {
        return Ok(());
    }
    let code = util::uuid();
    let body = json!([{"id": util::uuid(), "topic": "/subscriptions/00000000-0000-0000-0000-000000000000/resourceGroups/stuntman/providers/Microsoft.Storage/storageAccounts/stuntman",
        "subject": "", "data": {"validationCode": code, "validationUrl": ""}, "eventType": "Microsoft.EventGrid.SubscriptionValidationEvent",
        "eventTime": crate::auth::ms_iso(util::now_ms()), "metadataVersion": "1", "dataVersion": "2"}]);
    let r = ctx.http.post(&s.url).header("aeg-event-type", "SubscriptionValidation").header("aeg-subscription-name", s.name.to_uppercase()).json(&body).send().await;
    match r {
        Err(e) => Err(format!("the endpoint did not answer the validation handshake: {}", short_err(&e))),
        Ok(r) if !r.status().is_success() => Err(format!("the endpoint answered {} to the validation handshake", r.status().as_u16())),
        Ok(r) => {
            let v: Value = r.json().await.unwrap_or(Value::Null);
            if v["validationResponse"].as_str() == Some(code.as_str()) { Ok(()) } else { Err("the endpoint answered 200 but did not echo the validationCode".into()) }
        }
    }
}

fn short_err(e: &reqwest::Error) -> String {
    if e.is_timeout() { "timed out".into() } else if e.is_connect() { "connection refused".into() } else { e.to_string() }
}

/// One delivery attempt of a due event.
async fn deliver(ctx: &Ctx, e: &Value, s: &Sub) {
    let id = e["id"].as_str().unwrap_or("").to_string();
    let attempt = e["attempts"].as_i64().unwrap_or(0) + 1;
    let mut payload = e["payload"].clone();
    let behaviour = e["behaviour"].as_str().unwrap_or("").to_string();
    if behaviour == "missing_field" {
        remove_field(&s.format, &mut payload);
    }
    let t0 = std::time::Instant::now();
    let mut req = ctx.http.post(&s.url).json(&payload).timeout(std::time::Duration::from_secs(30));
    if s.format == "event_grid" {
        req = req.header("aeg-event-type", "Notification").header("aeg-subscription-name", s.name.to_uppercase()).header("aeg-delivery-count", (attempt - 1).to_string()).header("aeg-data-version", "").header("aeg-metadata-version", "1");
    }
    let r = req.send().await;
    let ms = t0.elapsed().as_millis() as i64;
    let (status, err) = match &r {
        Ok(r) => (Some(r.status().as_u16() as i64), String::new()),
        Err(x) => (None, short_err(x)),
    };
    let ok = status.map(|s| (200..300).contains(&s)).unwrap_or(false);
    let first = e["at"].as_i64().unwrap_or(0);
    let give_up = !ok && (attempt >= 30 || util::now_ms() - first > 86_400_000);
    let next = if ok || give_up { None } else { Some(util::now_ms() + backoff_ms(attempt)) };
    let new_status = if ok { "delivered" } else if give_up { "given_up" } else { "retrying" };
    let mut hist = e["history"].as_array().cloned().unwrap_or_default();
    hist.push(json!({"at": util::now_ms(), "attempt": attempt, "status": status, "ms": ms, "error": err, "url": s.url, "nextAt": next, "behaviour": behaviour}));
    let dup = ok && behaviour == "duplicate";
    if dup {
        let _ = ctx.http.post(&s.url).json(&payload).send().await;
        hist.push(json!({"at": util::now_ms(), "attempt": attempt, "status": status, "ms": 0, "error": "", "url": s.url, "duplicate": true}));
    }
    let h = json!(hist).to_string();
    let _ = ctx
        .db
        .call(move |cn| {
            cn.execute(
                "UPDATE storage_events SET status=?2, attempts=?3, next_at=?4, last_status=?5, last_ms=?6, history=?7, behaviour=CASE WHEN ?2='retrying' THEN '' ELSE behaviour END WHERE id=?1",
                params![id, new_status, attempt, next, status, ms, h],
            )
        })
        .await;
    ctx.live.send("storage.event", e["id"].as_str().unwrap_or(""), "updated");
}

/// Applies the armed "next delivery" of a subscription to a new event. Returns false if dropped.
async fn arm(ctx: &Ctx, e: &Value, s: &Sub) -> bool {
    let (_, _, next) = sub_state(ctx, &s.name).await.unwrap_or_default();
    if next.is_empty() || next == "normal" || e["attempts"].as_i64().unwrap_or(0) > 0 || !e["behaviour"].as_str().unwrap_or("").is_empty() {
        return true;
    }
    let _ = set_sub_state(ctx, &s.name, None, None, Some(String::new())).await;
    let id = e["id"].as_str().unwrap_or("").to_string();
    let (status, at) = match next.as_str() {
        "drop" => ("dropped", None),
        "reorder" => ("held", None),
        h if h.starts_with("hold:") => ("pending", Some(util::now_ms() + util::parse_ms(&h[5..]).unwrap_or(30_000) as i64)),
        _ => ("pending", Some(util::now_ms())),
    };
    let b = next.clone();
    let _ = ctx.db.call(move |cn| cn.execute("UPDATE storage_events SET behaviour=?2, status=?3, next_at=coalesce(?4, next_at) WHERE id=?1", params![id, b, status, at])).await;
    status != "dropped" && status != "held" && at.map(|a| a <= util::now_ms()).unwrap_or(true)
}

/// The delivery loop of one environment: due events, one at a time per subscription order.
pub async fn tick(ctx: &Ctx) {
    let due = ctx
        .db
        .call(|cn| {
            let mut s = cn.prepare(&format!("SELECT {COLS} FROM storage_events WHERE status IN ('pending','retrying') AND next_at <= ?1 ORDER BY at LIMIT 20"))?;
            let v = s.query_map([util::now_ms()], ev)?.collect::<Result<Vec<_>, _>>();
            v
        })
        .await
        .unwrap_or_default();
    if due.is_empty() {
        return;
    }
    let all = subs(ctx);
    for e in due {
        let Some(s) = all.iter().find(|s| s.name == e["subscription"]) else { continue };
        if !arm(ctx, &e, s).await {
            continue;
        }
        let fresh = get(ctx, e["id"].as_str().unwrap_or("")).await.unwrap_or(e.clone());
        if fresh["status"] != "pending" && fresh["status"] != "retrying" || fresh["nextAt"].as_i64().unwrap_or(0) > util::now_ms() {
            continue;
        }
        deliver(ctx, &fresh, s).await;
        // "Deliver after the next one": a held event goes right after this one.
        let sub = s.name.clone();
        let held = ctx
            .db
            .call(move |cn| cn.query_row("SELECT id FROM storage_events WHERE sub=?1 AND status='held' ORDER BY at LIMIT 1", [sub], |r| r.get::<_, String>(0)).optional())
            .await
            .ok()
            .flatten();
        if let Some(h) = held {
            if h != fresh["id"].as_str().unwrap_or("") {
                let _ = ctx.db.call(move |cn| cn.execute("UPDATE storage_events SET status='pending', next_at=?2 WHERE id=?1", params![h, util::now_ms()])).await;
            }
        }
    }
}

/// Puts an event back in the queue (resend), or sends it again to every subscription.
pub async fn resend(ctx: &Ctx, id: &str, to_all: bool, by: &str) -> Result<Vec<String>, ApiError> {
    let e = get(ctx, id).await?;
    if to_all {
        let mut out = vec![];
        for s in subs(ctx) {
            let mut p = e["payload"].clone();
            let nid = util::uuid();
            if let Some(x) = p.pointer_mut("/0/id") {
                *x = json!(nid);
            }
            let (sn, i, t, a, c, pa, b) = (s.name.clone(), nid.clone(), e["type"].as_str().unwrap_or("").to_string(), e["account"].as_str().unwrap_or("").to_string(), e["container"].as_str().unwrap_or("").to_string(), e["path"].as_str().unwrap_or("").to_string(), by.to_string());
            ctx.db
                .call(move |cn| cn.execute("INSERT INTO storage_events(id, sub, type, account, container, path, payload, at, by_who, status, next_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,'pending',?8)", params![i, sn, t, a, c, pa, p.to_string(), util::now_ms(), b]).map(|_| ()))
                .await?;
            out.push(nid);
        }
        return Ok(out);
    }
    let i = id.to_string();
    ctx.db.call(move |cn| cn.execute("UPDATE storage_events SET status='pending', next_at=?2, attempts=0, behaviour='' WHERE id=?1", params![i, util::now_ms()]).map(|_| ())).await?;
    ctx.live.send("storage.event", id, "updated");
    Ok(vec![id.to_string()])
}

pub fn validate(s: &Sub) -> Result<(), String> {
    if s.name.trim().is_empty() {
        return Err("name: required".into());
    }
    if s.to == "webhook" && !(s.url.starts_with("http://") || s.url.starts_with("https://")) {
        return Err("url: http:// or https://".into());
    }
    if s.to == "queue" && s.entity.is_empty() {
        return Err("entity: namespace/topic".into());
    }
    if !["webhook", "queue"].contains(&s.to.as_str()) {
        return Err("to: webhook | queue".into());
    }
    if !["event_grid", "s3"].contains(&s.format.as_str()) {
        return Err("format: event_grid | s3".into());
    }
    if s.events.is_empty() || s.events.iter().any(|e| e != "created" && e != "deleted") {
        return Err("events: created, deleted".into());
    }
    let _ = config::AZURE;
    Ok(())
}

pub fn save(ctx: &Ctx, all: Vec<Sub>) -> Result<(), ApiError> {
    for s in &all {
        validate(s).map_err(ApiError::bad)?;
    }
    yamlfile::save(&file(ctx), &File { subscriptions: all })?;
    ctx.live.send("storage.subscription", "", "updated");
    Ok(())
}
