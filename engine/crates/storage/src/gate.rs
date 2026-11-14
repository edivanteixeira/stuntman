//! What every request goes through before the operation, whatever the protocol: the account made
//! unreachable, the storage limit, a next read armed on the object, the first rule that fires,
//! and the account's latency. The protocol shapes the failure in its own words (XML, codes).

use std::time::Duration;

use stuntman_core::{ApiError, Ctx};

use crate::{
    config::{self, Account},
    rules::{self, Seen},
    store,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    Status(u16),
    Timeout,
    Reset,
    Empty,
}

#[derive(Clone, Debug, Default)]
pub struct Mods {
    pub slow_ms: u64,
    pub truncate: bool,
    pub bad_md5: bool,
    /// Why the answer is not the normal one ("rule · Contract uploads fail twice").
    pub provoked: Option<String>,
    /// A link rule: links expire this many seconds after they were issued.
    pub link_expires: Option<u64>,
}

fn effect(respond: &str, seconds: Option<u64>, m: &mut Mods) -> Option<Fail> {
    match respond {
        "timeout" => Some(Fail::Timeout),
        "reset" => Some(Fail::Reset),
        "empty" => Some(Fail::Empty),
        "slow" => {
            m.slow_ms = seconds.unwrap_or(5) * 1000;
            None
        }
        s if s.starts_with("slow:") => {
            m.slow_ms = stuntman_core::util::parse_ms(&s[5..]).unwrap_or(5000);
            None
        }
        "truncate" => {
            m.truncate = true;
            None
        }
        "bad_md5" => {
            m.bad_md5 = true;
            None
        }
        "expires" => {
            m.link_expires = seconds;
            None
        }
        "normal" | "" => None,
        s => s.parse().ok().map(Fail::Status),
    }
}

/// Ok(mods) to go on; Err((failure, why)) to answer with it.
pub async fn check(ctx: &Ctx, a: &Account, s: &Seen) -> Result<Result<Mods, (Fail, String)>, ApiError> {
    let mut m = Mods::default();
    if let Some((ms, jitter)) = config::latency_ms(&a.behaviour.latency) {
        if ms > 0 {
            let j = if jitter > 0 { rand::random::<u64>() % (2 * jitter) } else { jitter };
            tokio::time::sleep(Duration::from_millis((ms + j).saturating_sub(jitter))).await;
        }
    }
    if let Some(r) = store::reach(ctx, &a.account).await? {
        let mode = r["mode"].as_str().unwrap_or("").to_string();
        let f = match mode.as_str() {
            "timeout" => Fail::Timeout,
            "401" => Fail::Status(401),
            _ => Fail::Status(503),
        };
        return Ok(Err((f, format!("account made unreachable ({mode})"))));
    }
    if s.op == "put" {
        let limit = a.limit_bytes().unwrap_or(u64::MAX);
        if limit == 0 || (limit != u64::MAX && store::used_bytes(ctx, &a.account).await? as u64 + s.size > limit) {
            return Ok(Err((Fail::Status(507), "storage limit".into())));
        }
    }
    if s.op == "get" || s.op == "head" {
        if let Some((result, by)) = store::take_next(ctx, &a.account, &s.container, &s.path).await? {
            let why = format!("next read · set by {}", by.trim_start_matches("api:").trim_start_matches("mcp:"));
            if let Some(f) = effect(&result, None, &mut m) {
                return Ok(Err((f, why)));
            }
            m.provoked = Some(why);
        }
    }
    if let Some(r) = rules::decide(ctx, &a.kind, s).await? {
        let why = format!("rule · {}", r.name);
        if let Some(f) = effect(&r.then.respond, r.then.seconds, &mut m) {
            return Ok(Err((f, why)));
        }
        if m.provoked.is_none() && r.then.respond != "expires" {
            m.provoked = Some(why);
        }
    }
    if m.slow_ms > 0 {
        tokio::time::sleep(Duration::from_millis(m.slow_ms)).await;
    }
    Ok(Ok(m))
}
