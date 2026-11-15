//! The environment's clock (A10c): real time plus an offset you move with advance_clock, so a
//! subscription that renews in 30 days or a tracking that moves every 30 minutes can be tested now.
//! Kept in <env>/clock.json; templates read it as now().

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use crate::{util, Ctx};

fn cache() -> &'static Mutex<HashMap<PathBuf, i64>> {
    static C: OnceLock<Mutex<HashMap<PathBuf, i64>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn file(ctx: &Ctx) -> PathBuf {
    ctx.path("clock.json")
}

/// The offset in ms (0: real time).
pub fn offset(ctx: &Ctx) -> i64 {
    let f = file(ctx);
    let mut c = cache().lock().unwrap();
    if let Some(o) = c.get(&f) {
        return *o;
    }
    let o = std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|v| v["offset_ms"].as_i64()).unwrap_or(0);
    c.insert(f, o);
    o
}

/// The environment's time in ms.
pub fn now(ctx: &Ctx) -> i64 {
    util::now_ms() + offset(ctx)
}

fn set(ctx: &Ctx, o: i64) -> i64 {
    let f = file(ctx);
    if o == 0 {
        let _ = std::fs::remove_file(&f);
    } else {
        let _ = std::fs::write(&f, serde_json::json!({"offset_ms": o}).to_string());
    }
    cache().lock().unwrap().insert(f, o);
    o
}

/// Moves the clock forward (never back past real time).
pub fn advance(ctx: &Ctx, ms: i64) -> i64 {
    set(ctx, (offset(ctx) + ms).max(0))
}

pub fn reset(ctx: &Ctx) -> i64 {
    set(ctx, 0)
}
