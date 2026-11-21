//! Every login and transfer on the file servers.

use rusqlite::params;
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS files_log(seq INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL, server TEXT NOT NULL, user TEXT NOT NULL DEFAULT '', proto TEXT NOT NULL,
  op TEXT NOT NULL, path TEXT NOT NULL DEFAULT '', size INTEGER NOT NULL DEFAULT 0, ok INTEGER NOT NULL DEFAULT 1, detail TEXT NOT NULL DEFAULT '');
CREATE INDEX IF NOT EXISTS files_log_at ON files_log(server, at);
";

#[allow(clippy::too_many_arguments)]
pub async fn log(ctx: &Ctx, server: &str, user: &str, proto: &str, op: &str, path: &str, size: u64, ok: bool, detail: &str) {
    let (s, u, pr, o, p, d) = (server.to_string(), user.to_string(), proto.to_string(), op.to_string(), path.to_string(), detail.to_string());
    let _ = ctx.db.call(move |cn| cn.execute("INSERT INTO files_log(at, server, user, proto, op, path, size, ok, detail) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)", params![util::now_ms(), s, u, pr, o, p, size as i64, ok, d]).map(|_| ())).await;
    ctx.live.send("files", server, "row");
    if ["put", "get", "delete", "login"].contains(&op) {
        let id = util::id("ft");
        let label = format!("{proto} {op} {path}{}", if size > 0 { format!(" · {size} B") } else { String::new() });
        let _ = stuntman_core::trips::record(ctx, stuntman_core::trips::Trip { id: &id, module: "files", label: &label, status: if ok { "ok" } else { "failed" }, trace_id: None,
            link: &format!("/ftp?s={server}"), host: server, actor: user, outcome: if !ok && detail.contains("provoked") { "provoked" } else if ok { "ok" } else { "failed" }, detail }).await;
    }
}

pub async fn list(ctx: &Ctx, server: Option<String>, limit: i64) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |cn| {
            let mut st = cn.prepare("SELECT at, server, user, proto, op, path, size, ok, detail FROM files_log WHERE (?1 IS NULL OR server=?1) ORDER BY seq DESC LIMIT ?2")?;
            let v = st
                .query_map(params![server, limit], |r| Ok(json!({"at": r.get::<_, i64>(0)?, "server": r.get::<_, String>(1)?, "user": r.get::<_, String>(2)?, "proto": r.get::<_, String>(3)?,
                    "op": r.get::<_, String>(4)?, "path": r.get::<_, String>(5)?, "size": r.get::<_, i64>(6)?, "ok": r.get::<_, bool>(7)?, "detail": r.get::<_, String>(8)?})))?
                .filter_map(|x| x.ok())
                .collect();
            Ok(v)
        })
        .await
}
