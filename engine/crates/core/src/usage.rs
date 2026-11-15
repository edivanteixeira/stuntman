//! Usage: how many calls each service kind got, per environment and day, so the rail can put the
//! services an environment's apps use first. Counted in memory (cheap on hot paths: a Kafka request,
//! an OTLP batch), added to the environment's SQLite by the heartbeat every few seconds and on
//! shutdown. Every `Live::seen` counts one call; protocols that see a client only once per
//! connection (Kafka, IMAP, LDAP, ActiveMQ, Event Hubs) also call [`hit`] per request.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use rusqlite::params;
use serde_json::{json, Map, Value};

use crate::{store::Db, util, ApiError, Ctx};

pub const SQL: &str = "CREATE TABLE IF NOT EXISTS usage(kind TEXT NOT NULL, day INTEGER NOT NULL, calls INTEGER NOT NULL, last_at INTEGER NOT NULL, PRIMARY KEY(kind, day));";

/// The rail's service kinds.
pub const KINDS: [&str; 16] = ["email", "whatsapp", "sms", "storage", "queues", "streams", "pubsub", "kinesis", "files", "keyvault", "identity", "apis", "ai", "events", "webhooks", "telemetry"];

const DAY: i64 = 86_400_000;
/// A call this recent makes the service "live" (the green dot).
const LIVE_MS: i64 = 5 * 60_000;

/// Calls not yet written: (kind, UTC day) → (calls, last at).
#[derive(Clone, Default)]
pub struct Counter(Arc<Mutex<HashMap<(&'static str, i64), (i64, i64)>>>);

impl Counter {
    pub fn hit(&self, kind: &'static str) {
        self.add(kind, util::now_ms(), 1);
    }
    fn add(&self, kind: &'static str, at: i64, n: i64) {
        let mut m = self.0.lock().unwrap();
        let e = m.entry((kind, at / DAY)).or_default();
        e.0 += n;
        e.1 = e.1.max(at);
    }
}

/// One call to a service kind of this environment (one of [`KINDS`]).
pub fn hit(ctx: &Ctx, kind: &'static str) {
    ctx.live.usage.hit(kind);
}

/// The kind of a client's "via" (see `Live::seen`); MCP and API are you and your tests, not the app.
pub fn kind_of_via(via: &str) -> Option<&'static str> {
    Some(match via {
        "SMTP" | "SendGrid API" | "IMAP" | "POP3" => "email",
        "WhatsApp API" => "whatsapp",
        "Zenvia SMS" => "sms",
        "Storage" => "storage",
        "Service Bus" | "RabbitMQ" | "Event Hubs" | "ActiveMQ" => "queues",
        "Streams" => "streams",
        "Pub/Sub" => "pubsub",
        "Kinesis" => "kinesis",
        "Key Vault" => "keyvault",
        "Identity" => "identity",
        "APIs" => "apis",
        "AI models" => "ai",
        "Events" => "events",
        "Webhooks" => "webhooks",
        "OTLP" => "telemetry",
        _ => return None,
    })
}

/// The kind of a Timeline (trips) module.
fn kind_of_module(m: &str) -> Option<&'static str> {
    Some(match m {
        "webhook" => "webhooks",
        "messaging" => "queues",
        m => KINDS.into_iter().find(|k| *k == m)?,
    })
}

/// Writes the pending counts. On a failed write they go back to memory.
pub async fn flush_into(db: &Db, c: &Counter) -> Result<(), ApiError> {
    let m = std::mem::take(&mut *c.0.lock().unwrap());
    if m.is_empty() {
        return Ok(());
    }
    let rows: Vec<_> = m.iter().map(|(&(k, d), &(n, l))| (k, d, n, l)).collect();
    let old = util::now_ms() / DAY - 30;
    let r = db
        .call(move |cn| {
            let tx = cn.transaction()?;
            for (k, d, n, l) in &rows {
                tx.execute("INSERT INTO usage(kind, day, calls, last_at) VALUES(?1,?2,?3,?4) ON CONFLICT(kind, day) DO UPDATE SET calls = calls + ?3, last_at = max(last_at, ?4)", params![k, d, n, l])?;
            }
            tx.execute("DELETE FROM usage WHERE day < ?1", [old])?;
            tx.commit()
        })
        .await;
    if r.is_err() {
        for ((k, _), (n, l)) in m {
            c.add(k, l, n);
        }
    }
    r
}

pub async fn flush(ctx: &Ctx) -> Result<(), ApiError> {
    flush_into(&ctx.db, &ctx.live.usage).await
}

/// Per kind: calls today, calls in the last 7 days, the last call and whether it was in the last 5
/// minutes. Rows: (kind, day, calls, last at) from the counter and from the Timeline; the two see the
/// same calls where both exist, so a day takes the larger, never the sum.
pub fn merge(counted: &[(String, i64, i64, i64)], trips: &[(String, i64, i64, i64)], now: i64) -> Value {
    let today = now / DAY;
    let mut days: HashMap<(&str, i64), (i64, i64)> = HashMap::new();
    let all = counted.iter().filter_map(|(k, d, n, l)| Some((KINDS.into_iter().find(|x| x == k)?, *d, *n, *l)))
        .chain(trips.iter().filter_map(|(m, d, n, l)| Some((kind_of_module(m)?, *d, *n, *l))));
    for (k, d, n, l) in all.filter(|r| r.1 > today - 7) {
        let e = days.entry((k, d)).or_default();
        *e = (e.0.max(n), e.1.max(l));
    }
    let mut out = Map::new();
    for k in KINDS {
        let (mut t, mut w, mut last) = (0, 0, 0);
        for (&(_, d), &(n, l)) in days.iter().filter(|((x, _), _)| *x == k) {
            w += n;
            if d == today { t += n; }
            last = last.max(l);
        }
        out.insert(k.into(), json!({"today": t, "week": w, "lastAt": (last > 0).then_some(last), "live": last > now - LIVE_MS}));
    }
    Value::Object(out)
}

/// The `usage` tool's answer for this environment.
pub async fn summary(ctx: &Ctx) -> Result<Value, ApiError> {
    flush(ctx).await?;
    let now = util::now_ms();
    let from = now / DAY - 6;
    let (counted, trips) = ctx
        .db
        .call(move |c| {
            let q = |sql: &str, p: i64| -> rusqlite::Result<Vec<(String, i64, i64, i64)>> {
                let mut st = c.prepare(sql)?;
                let v = st.query_map([p], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect();
                v
            };
            // Timeline rows by the app (not you, an agent or a test acting through tools).
            Ok((q("SELECT kind, day, calls, last_at FROM usage WHERE day >= ?1", from)?,
                q("SELECT module, at / 86400000, count(*), max(at) FROM trips WHERE at >= ?1 AND actor <> 'you' AND actor NOT LIKE 'mcp:%' AND actor NOT LIKE 'api:%' GROUP BY 1, 2", from * DAY)?))
        })
        .await?;
    Ok(merge(&counted, &trips, now))
}

#[cfg(test)]
mod t {
    use super::*;

    #[tokio::test]
    async fn counts_survive_a_flush_and_trips_do_not_double() {
        let db = Db::memory();
        db.migrate(SQL).unwrap();
        let c = Counter::default();
        for _ in 0..3 { c.hit("streams"); }
        c.hit("email");
        flush_into(&db, &c).await.unwrap();
        c.hit("streams");
        flush_into(&db, &c).await.unwrap();
        let now = util::now_ms();
        let rows: Vec<(String, i64, i64, i64)> = db.call(|cn| cn.prepare("SELECT kind, day, calls, last_at FROM usage")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect()).await.unwrap();
        let today = now / DAY;
        // The Timeline saw 2 of today's e-mails and 5 queue messages yesterday.
        let trips = vec![("email".to_string(), today, 2, now - 1000), ("messaging".to_string(), today - 1, 5, now - DAY), ("messaging".to_string(), today - 9, 7, now - 9 * DAY)];
        let v = merge(&rows, &trips, now);
        assert_eq!(v["streams"]["today"], 4);
        assert_eq!(v["streams"]["live"], true);
        assert_eq!(v["email"]["today"], 2);
        assert_eq!(v["queues"]["today"], 0);
        assert_eq!(v["queues"]["week"], 5);
        assert_eq!(v["queues"]["live"], false);
        assert_eq!(v["sms"]["week"], 0);
        assert!(v["sms"]["lastAt"].is_null());
    }
}
