//! Forced failures of a provider's send endpoint (SMS, WhatsApp): the next N sends answer as the
//! real provider does when it fails, at the HTTP level: 500, 400 with its error body, or the
//! connection itself (reset, empty, timeout). The table lives with the trips (trips::SQL).

use std::time::Duration;

use axum::{http::StatusCode, response::{IntoResponse, Response}};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};

use crate::{netserve::Kill, util, ApiError, Ctx};

pub const ANSWERS: [&str; 5] = ["500", "400", "reset", "empty", "timeout"];

/// Forces the next `count` sends of `module`; "normal" stops it.
pub async fn set(ctx: &Ctx, module: &'static str, answer: &str, count: u64, by: &str) -> Result<Option<Value>, ApiError> {
    if answer != "normal" && !ANSWERS.contains(&answer) {
        return Err(ApiError::bad(format!("answer: {} | normal", ANSWERS.join(" | "))));
    }
    let (a, b) = (answer.to_string(), by.to_string());
    ctx.db.call(move |c| if a == "normal" {
        c.execute("DELETE FROM forced_sends WHERE module=?1", [module]).map(|_| ())
    } else {
        c.execute("INSERT OR REPLACE INTO forced_sends VALUES(?1,?2,?3,?4,?5)", params![module, a, count.max(1) as i64, b, util::now_ms()]).map(|_| ())
    }).await?;
    ctx.live.send(&format!("{module}.force"), "", "updated");
    get(ctx, module).await
}

pub async fn get(ctx: &Ctx, module: &'static str) -> Result<Option<Value>, ApiError> {
    Ok(ctx.db.call(move |c| c.query_row("SELECT answer, left, by_who, at FROM forced_sends WHERE module=?1 AND left>0", [module],
        |r| Ok(json!({"answer": r.get::<_, String>(0)?, "left": r.get::<_, i64>(1)?, "by": r.get::<_, String>(2)?, "at": r.get::<_, i64>(3)?}))).optional()).await?)
}

/// Takes one forced answer, if any: (answer, who forced it).
pub async fn take(ctx: &Ctx, module: &'static str) -> Option<(String, String)> {
    let f = get(ctx, module).await.ok().flatten()?;
    let _ = ctx.db.call(move |c| c.execute("UPDATE forced_sends SET left=left-1 WHERE module=?1", [module]).map(|_| ())).await;
    ctx.live.send(&format!("{module}.force"), "", "updated");
    Some((f["answer"].as_str()?.to_string(), f["by"].as_str().unwrap_or("").to_string()))
}

/// The connection itself fails: reset (TCP RST), empty (closed without a byte) or timeout.
pub async fn network(answer: &str, kill: Option<Kill>) -> Response {
    if answer == "timeout" {
        tokio::time::sleep(Duration::from_secs(600)).await;
        return StatusCode::GATEWAY_TIMEOUT.into_response();
    }
    if let Some(k) = kill {
        k.fire(answer == "reset");
        std::future::pending::<()>().await;
    }
    StatusCode::BAD_GATEWAY.into_response()
}
