//! Every event the app sent, normalized, and the failures forced on the collectors.

use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS ev_events(
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT UNIQUE NOT NULL, at INTEGER NOT NULL, source TEXT NOT NULL, kind TEXT NOT NULL, name TEXT NOT NULL,
  user TEXT NOT NULL DEFAULT '', props TEXT NOT NULL DEFAULT '{}', pii TEXT NOT NULL DEFAULT '[]', caller TEXT NOT NULL DEFAULT '', sent_at TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS ev_events_name ON ev_events(name, at);
CREATE INDEX IF NOT EXISTS ev_events_user ON ev_events(user, at);
CREATE TABLE IF NOT EXISTS ev_force(seq INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL, kind TEXT NOT NULL, left INTEGER NOT NULL, by_who TEXT NOT NULL, at INTEGER NOT NULL);
";

#[derive(Debug, Clone, Default)]
pub struct Ev {
    pub source: String,
    pub kind: String,
    pub name: String,
    pub user: String,
    pub props: Value,
    pub pii: Vec<String>,
    pub sent_at: String,
}

pub async fn add(ctx: &Ctx, evs: Vec<Ev>, caller: &str) -> Result<usize, ApiError> {
    let n = evs.len();
    let c = caller.to_string();
    ctx.db
        .call(move |cn| {
            let tx = cn.transaction()?;
            {
                let mut st = tx.prepare("INSERT INTO ev_events(id, at, source, kind, name, user, props, pii, caller, sent_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?;
                for e in evs {
                    st.execute(params![util::id("ev"), util::now_ms(), e.source, e.kind, e.name, e.user, e.props.to_string(), json!(e.pii).to_string(), c, e.sent_at])?;
                }
            }
            tx.commit()
        })
        .await?;
    if n > 0 {
        ctx.live.send("events", "*", "created");
    }
    Ok(n)
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "source": r.get::<_, String>(2)?, "kind": r.get::<_, String>(3)?, "name": r.get::<_, String>(4)?,
        "user": r.get::<_, String>(5)?, "props": serde_json::from_str::<Value>(&r.get::<_, String>(6)?).unwrap_or(Value::Null),
        "pii": serde_json::from_str::<Value>(&r.get::<_, String>(7)?).unwrap_or(json!([])), "caller": r.get::<_, String>(8)?}))
}

pub async fn list(ctx: &Ctx, source: Option<String>, name: Option<String>, user: Option<String>, since: i64, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT id, at, source, kind, name, user, props, pii, caller FROM ev_events WHERE at>=?1 AND (?2 IS NULL OR source=?2) AND (?3 IS NULL OR name=?3) AND (?4 IS NULL OR user=?4) ORDER BY seq DESC LIMIT ?5")?;
            let v = st.query_map(params![since, source, name, user, limit], row)?.filter_map(|x| x.ok()).collect();
            Ok(v)
        })
        .await
}

pub async fn people(ctx: &Ctx, since: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT user, COUNT(*), MAX(at), GROUP_CONCAT(DISTINCT source) FROM ev_events WHERE at>=?1 AND user<>'' GROUP BY user ORDER BY MAX(at) DESC LIMIT 500")?;
            let v = st.query_map([since], |r| Ok(json!({"user": r.get::<_, String>(0)?, "events": r.get::<_, i64>(1)?, "last": r.get::<_, i64>(2)?, "sources": r.get::<_, String>(3)?})))?.filter_map(|x| x.ok()).collect();
            Ok(v)
        })
        .await
}

pub async fn force(ctx: &Ctx, source: &str, kind: &str, count: i64, by: &str) -> Result<(), ApiError> {
    let (s, k, b) = (source.to_string(), kind.to_string(), by.to_string());
    ctx.db
        .call(move |cn| {
            if k == "none" {
                cn.execute("DELETE FROM ev_force WHERE source=?1 OR ?1=''", [s]).map(|_| ())
            } else {
                cn.execute("INSERT INTO ev_force(source, kind, left, by_who, at) VALUES(?1,?2,?3,?4,?5)", params![s, k, count, b, util::now_ms()]).map(|_| ())
            }
        })
        .await?;
    ctx.live.send("events.force", source, "updated");
    Ok(())
}

pub async fn take_force(ctx: &Ctx, source: &str) -> Result<Option<String>, ApiError> {
    let s = source.to_string();
    ctx.db
        .call(move |cn| {
            let f: Option<(i64, String)> = cn.query_row("SELECT seq, kind FROM ev_force WHERE left>0 AND (source=?1 OR source='') ORDER BY seq LIMIT 1", [s], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
            if let Some((seq, _)) = &f {
                cn.execute("UPDATE ev_force SET left=left-1 WHERE seq=?1", [seq])?;
                cn.execute("DELETE FROM ev_force WHERE left<=0", [])?;
            }
            Ok(f.map(|x| x.1))
        })
        .await
}
