//! Deliveries (H1): every request an inbox caught, with the checks and what it answered.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, OptionalExtension, Row};
use serde_json::{json, Value};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS webhook_deliveries(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE, inbox TEXT NOT NULL, at INTEGER NOT NULL, method TEXT NOT NULL,
  path TEXT NOT NULL, headers TEXT NOT NULL, body BLOB NOT NULL, size INTEGER NOT NULL, event_type TEXT NOT NULL DEFAULT '',
  event_id TEXT NOT NULL DEFAULT '', label TEXT NOT NULL DEFAULT '', attempt INTEGER NOT NULL DEFAULT 1, signature TEXT NOT NULL DEFAULT '{}',
  duplicate_of TEXT, answer TEXT NOT NULL DEFAULT '', answer_ms INTEGER, gave_up_ms INTEGER, traps TEXT NOT NULL DEFAULT '[]',
  remote TEXT NOT NULL DEFAULT '', user_agent TEXT NOT NULL DEFAULT '', trace_id TEXT, done INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS webhook_deliveries_inbox ON webhook_deliveries(inbox, at);
CREATE INDEX IF NOT EXISTS webhook_deliveries_event ON webhook_deliveries(inbox, event_id);
CREATE TABLE IF NOT EXISTS webhook_ephemeral(name TEXT PRIMARY KEY, data TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER);
CREATE TABLE IF NOT EXISTS webhook_counts(inbox TEXT NOT NULL, key TEXT NOT NULL, n INTEGER NOT NULL, PRIMARY KEY(inbox, key));
CREATE TABLE IF NOT EXISTS webhook_gone(name TEXT PRIMARY KEY, at INTEGER NOT NULL);
";

const COLS: &str = "id, seq, inbox, at, method, path, headers, size, event_type, event_id, label, attempt, signature, duplicate_of, answer,
  answer_ms, gave_up_ms, traps, remote, user_agent, trace_id, done";

fn row(r: &Row) -> rusqlite::Result<Value> {
    let j = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    Ok(json!({
        "id": r.get::<_, String>(0)?, "seq": r.get::<_, i64>(1)?, "inbox": r.get::<_, String>(2)?, "at": r.get::<_, i64>(3)?,
        "method": r.get::<_, String>(4)?, "path": r.get::<_, String>(5)?, "headers": j(6)?, "size": r.get::<_, i64>(7)?,
        "eventType": r.get::<_, String>(8)?, "eventId": r.get::<_, String>(9)?, "label": r.get::<_, String>(10)?,
        "attempt": r.get::<_, i64>(11)?, "signature": j(12)?, "duplicateOf": r.get::<_, Option<String>>(13)?.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
        "answer": r.get::<_, String>(14)?, "answerMs": r.get::<_, Option<i64>>(15)?, "gaveUpMs": r.get::<_, Option<i64>>(16)?,
        "traps": j(17)?, "remote": r.get::<_, String>(18)?, "userAgent": r.get::<_, String>(19)?, "traceId": r.get::<_, Option<String>>(20)?,
        "done": r.get::<_, bool>(21)?,
    }))
}

/// "order.completed", the event id, and a short label such as "#58213" or "INV-2207".
pub fn event_of(headers: &[(String, String)], body: &[u8]) -> (String, String, String) {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let hdr = |part: &str| headers.iter().find(|(k, _)| k.to_lowercase().contains(part)).map(|(_, v)| v.trim().to_string());
    // Keys in any case (some apps send "Action": 1 with "ActionDescription": "DocumentSigned").
    let get = |o: &Value, k: &str| o.as_object().and_then(|m| m.iter().find(|(x, _)| x.eq_ignore_ascii_case(k)).map(|(_, x)| x.clone()));
    let ty = ["type", "event", "event_type", "eventType", "topic", "action"]
        .iter()
        .find_map(|k| match get(&v, k) {
            Some(Value::String(s)) => Some(s),
            Some(Value::Number(n)) => Some(get(&v, &format!("{k}Description")).and_then(|d| d.as_str().map(String::from)).unwrap_or(n.to_string())),
            _ => None,
        })
        .or_else(|| headers.iter().find(|(k, _)| { let k = k.to_lowercase(); k.ends_with("-event") || k.ends_with("-topic") }).map(|(_, v)| v.clone()))
        .unwrap_or_default();
    let id = hdr("event-id").or_else(|| hdr("webhook-id")).or_else(|| hdr("delivery-id")).or_else(|| hdr("delivery"))
        .or_else(|| get(&v, "id").map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())))
        .unwrap_or_default();
    let data = get(&v, "data").map(|d| get(&d, "object").unwrap_or(d)).unwrap_or(v.clone());
    let label = ["order", "number", "invoice", "invoice_id", "order_id", "reference", "id"]
        .iter()
        .find_map(|k| get(&data, k).filter(|x| x.is_string() || x.is_number()))
        .map(|x| match x {
            Value::Number(n) => format!("#{n}"),
            Value::String(s) if s.chars().all(|c| c.is_ascii_digit()) => format!("#{s}"),
            Value::String(s) => s.clone(),
            o => o.to_string(),
        })
        .filter(|l| Some(l.trim_start_matches('#')) != v.get("id").and_then(|x| x.as_str()))
        .unwrap_or_default();
    (ty, id, label)
}

pub struct New {
    pub inbox: String,
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub signature: Value,
    pub remote: String,
}

/// Records the arrival; returns (id, attempt, duplicate_of).
pub async fn insert(ctx: &Ctx, n: New) -> Result<(String, i64, Option<Value>), ApiError> {
    let (ty, eid, label) = event_of(&n.headers, &n.body);
    let ua = n.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("user-agent")).map(|(_, v)| v.clone()).unwrap_or_default();
    let trace = n.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("traceparent")).and_then(|(_, v)| v.split('-').nth(1).map(String::from));
    let headers = json!(n.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>()).to_string();
    let (trace_for_trip, inbox_for_trip, ty_for_trip) = (trace.clone(), n.inbox.clone(), ty.clone());
    let size = n.body.len() as i64;
    let mut body = n.body;
    body.truncate(256 * 1024);
    let r = ctx
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            // Attempt n of the same event; a duplicate is a resend after the inbox already said yes.
            let (prev, dup): (i64, Option<(String, i64)>) = if eid.is_empty() {
                (0, None)
            } else {
                let prev = tx.query_row("SELECT COUNT(*) FROM webhook_deliveries WHERE inbox = ?1 AND event_id = ?2", params![n.inbox, eid], |r| r.get(0))?;
                let dup = tx
                    .query_row(
                        "SELECT id, at FROM webhook_deliveries WHERE inbox = ?1 AND event_id = ?2 AND answer GLOB '2*' ORDER BY seq DESC LIMIT 1",
                        params![n.inbox, eid],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                (prev, dup)
            };
            let dup_json = dup.map(|(id, at)| json!({"id": id, "at": at}));
            tx.execute(
                "INSERT INTO webhook_deliveries(inbox, at, method, path, headers, body, size, event_type, event_id, label, attempt, signature, duplicate_of, remote, user_agent, trace_id)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)",
                params![n.inbox, util::now_ms(), n.method, n.path, headers, body, size, ty, eid, label, prev + 1, n.signature.to_string(),
                        dup_json.as_ref().map(|d| d.to_string()), n.remote, ua, trace],
            )?;
            let seq = tx.last_insert_rowid();
            let id = format!("dlv_{seq:04}");
            tx.execute("UPDATE webhook_deliveries SET id = ?1 WHERE seq = ?2", params![id, seq])?;
            tx.commit()?;
            Ok((id, prev + 1, dup_json))
        })
        .await?;
    ctx.live.send("webhook.delivery", &r.0, "created");
    if let Some(t) = trace_for_trip {
        let label = format!("POST /in/{inbox_for_trip}  {ty_for_trip}");
        stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &r.0, module: "webhook", label: &label, status: "", trace_id: Some(&t), link: &format!("/inboxes/webhooks/{inbox_for_trip}"), host: "hooks" , actor: "", outcome: "ok", detail: "" }).await?;
    }
    Ok(r)
}

pub async fn finish(ctx: &Ctx, id: &str, answer: &str, answer_ms: Option<i64>, gave_up_ms: Option<i64>, traps: Value) -> Result<(), ApiError> {
    let (i, a, t) = (id.to_string(), answer.to_string(), traps.to_string());
    ctx.db
        .call(move |c| c.execute("UPDATE webhook_deliveries SET answer=?2, answer_ms=?3, gave_up_ms=?4, traps=?5, done=1 WHERE id=?1", params![i, a, answer_ms, gave_up_ms, t]))
        .await?;
    ctx.live.send("webhook.delivery", id, "updated");
    stuntman_core::trips::set_status(ctx, id, answer).await
}

/// Marks one trap on a delivery as passed or failed.
pub async fn set_trap(ctx: &Ctx, id: &str, trap: &str, result: &str, detail: &str) -> Result<(), ApiError> {
    let (i, tr, re, de) = (id.to_string(), trap.to_string(), result.to_string(), detail.to_string());
    ctx.db
        .call(move |c| {
            let cur: String = c.query_row("SELECT traps FROM webhook_deliveries WHERE id = ?1", [&i], |r| r.get(0))?;
            let mut v: Vec<Value> = serde_json::from_str(&cur).unwrap_or_default();
            v.retain(|x| x["trap"] != tr.as_str());
            v.push(json!({"trap": tr, "result": re, "detail": de}));
            c.execute("UPDATE webhook_deliveries SET traps = ?2 WHERE id = ?1", params![i, Value::Array(v).to_string()])
        })
        .await?;
    ctx.live.send("webhook.delivery", id, "updated");
    Ok(())
}

pub async fn count_and_bump(ctx: &Ctx, inbox: &str, key: &str) -> Result<i64, ApiError> {
    let (i, k) = (inbox.to_string(), key.to_string());
    ctx.db
        .call(move |c| {
            c.execute("INSERT INTO webhook_counts(inbox, key, n) VALUES(?1, ?2, 1) ON CONFLICT(inbox, key) DO UPDATE SET n = n + 1", params![i, k])?;
            c.query_row("SELECT n FROM webhook_counts WHERE inbox = ?1 AND key = ?2", params![i, k], |r| r.get(0))
        })
        .await
}

pub async fn reset_counts(ctx: &Ctx, inbox: &str) -> Result<(), ApiError> {
    let i = inbox.to_string();
    ctx.db.call(move |c| c.execute("DELETE FROM webhook_counts WHERE inbox = ?1", [i])).await?;
    Ok(())
}

pub async fn list(ctx: &Ctx, inbox: Option<String>, event: Option<String>, since: Option<i64>, limit: u32) -> Result<(Vec<Value>, i64), ApiError> {
    ctx.db
        .call(move |c| {
            let w = "(?1 IS NULL OR inbox = ?1) AND (?2 IS NULL OR event_type = ?2 OR event_id = ?2) AND (?3 IS NULL OR at >= ?3)";
            let total: i64 = c.query_row(&format!("SELECT COUNT(*) FROM webhook_deliveries WHERE {w}"), params![inbox, event, since], |r| r.get(0))?;
            let mut st = c.prepare(&format!("SELECT {COLS} FROM webhook_deliveries WHERE {w} ORDER BY seq DESC LIMIT {}", limit.clamp(1, 1000)))?;
            let v = st.query_map(params![inbox, event, since], row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((v, total))
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str) -> Result<Value, ApiError> {
    let i = id.to_string();
    let v = ctx
        .db
        .call(move |c| {
            let Some(mut d) = c.query_row(&format!("SELECT {COLS} FROM webhook_deliveries WHERE id = ?1"), [&i], row).optional()? else { return Ok(None) };
            let body: Vec<u8> = c.query_row("SELECT body FROM webhook_deliveries WHERE id = ?1", [&i], |r| r.get(0))?;
            d["body"] = json!(String::from_utf8_lossy(&body));
            let (inbox, eid) = (d["inbox"].as_str().unwrap_or("").to_string(), d["eventId"].as_str().unwrap_or("").to_string());
            if !eid.is_empty() {
                let mut st = c.prepare(&format!("SELECT {COLS} FROM webhook_deliveries WHERE inbox = ?1 AND event_id = ?2 ORDER BY seq"))?;
                d["attempts"] = json!(st.query_map(params![inbox, eid], row)?.collect::<rusqlite::Result<Vec<_>>>()?);
            }
            Ok(Some(d))
        })
        .await?;
    v.ok_or_else(|| ApiError::not_found(format!("No delivery {id}")))
}

pub async fn sweep(ctx: &Ctx) -> Result<(), ApiError> {
    ctx.db
        .call(|c| {
            let now = util::now_ms();
            c.execute("DELETE FROM webhook_deliveries WHERE at < ?1", [now - 7 * 86_400_000])?;
            // Expired ephemeral inboxes become "gone" (410) for a day, then disappear.
            c.execute("INSERT OR REPLACE INTO webhook_gone(name, at) SELECT name, expires_at FROM webhook_ephemeral WHERE expires_at < ?1", [now])?;
            c.execute("DELETE FROM webhook_ephemeral WHERE expires_at < ?1", [now])?;
            c.execute("DELETE FROM webhook_gone WHERE at < ?1", [now - 86_400_000])?;
            Ok(())
        })
        .await
}

#[cfg(test)]
mod t {
    #[test]
    fn event_label() {
        let b = br#"{"type":"order.completed","id":"evt_8d41e2b7","data":{"order":58213,"customer":"globex"}}"#;
        assert_eq!(super::event_of(&[], b), ("order.completed".into(), "evt_8d41e2b7".into(), "#58213".into()));
        let b = br#"{"event":"invoice.paid","data":{"invoice":"INV-2207"}}"#;
        let h = vec![("X-Acme-Event-Id".to_string(), "evt_1".to_string())];
        assert_eq!(super::event_of(&h, b), ("invoice.paid".into(), "evt_1".into(), "INV-2207".into()));
        // PascalCase with a numeric action and its description (an e-signature platform's shape).
        let b = br#"{"CompanyId":13467,"Action":1,"ActionDescription":"DocumentSigned","Data":{"Id":2760,"Status":5}}"#;
        assert_eq!(super::event_of(&[], b), ("DocumentSigned".into(), "".into(), "#2760".into()));
    }
}
