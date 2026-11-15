//! pubsub/config.yaml: the project tools use for short names and the failure provoked on Publish.

use serde::{Deserialize, Serialize};
use stuntman_core::{yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// Tools take "shipments" for projects/<project>/topics/shipments when no topic of that name exists.
    pub project: String,
    /// "" | unavailable | deadline_exceeded | resource_exhausted | not_found | slow_publish | duplicate | ack_transient
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provoke: String,
    /// Publish calls (ack/modack requests for ack_transient) the provocation still answers; 0 = until cleared.
    pub times: u32,
    /// Only Publish to this topic (short or full name); empty = every topic.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub topic: String,
    /// slow_publish: how long Publish waits.
    pub delay_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { project: "acme-stg".into(), provoke: String::new(), times: 0, topic: String::new(), delay_ms: 3000 }
    }
}

pub const PROVOKE: [(&str, &str); 7] = [("unavailable", "Publish answers UNAVAILABLE (clients retry)"), ("deadline_exceeded", "Publish answers DEADLINE_EXCEEDED"),
    ("resource_exhausted", "Publish answers RESOURCE_EXHAUSTED (quota)"), ("not_found", "Publish answers NOT_FOUND, as if the topic were deleted"),
    ("slow_publish", "Publish waits delay_ms before answering"), ("duplicate", "every message is delivered once more after its first ack"),
    ("ack_transient", "exactly-once acks and modacks fail with TRANSIENT_FAILURE_UNORDERED_ACK_ID (clients retry)")];

pub fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("pubsub/config.yaml")
}

pub fn load(ctx: &Ctx) -> Config {
    stuntman_core::yamlfile::load(&path(ctx)).unwrap_or_default()
}

pub fn save(ctx: &Ctx, c: &Config) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), c)
}

/// The provocation this Publish gets (kind, delay), counting it down. duplicate is not a Publish
/// failure: acks read it.
pub fn take(ctx: &Ctx, topic: &str) -> Option<(String, u64)> {
    let mut c = load(ctx);
    if c.provoke.is_empty() || c.provoke == "duplicate" || c.provoke == "ack_transient" || !(c.topic.is_empty() || c.topic == topic || topic.ends_with(&format!("/topics/{}", c.topic))) {
        return None;
    }
    let got = (c.provoke.clone(), c.delay_ms);
    if c.times > 0 {
        c.times -= 1;
        if c.times == 0 {
            c.provoke.clear();
        }
        let _ = save(ctx, &c);
        ctx.live.send("pubsub.config", "", "updated");
    }
    Some(got)
}

/// Whether this exactly-once ack or modack request fails with a transient error, counting it down.
pub fn take_ack(ctx: &Ctx) -> bool {
    let mut c = load(ctx);
    if c.provoke != "ack_transient" { return false }
    if c.times > 0 {
        c.times -= 1;
        if c.times == 0 { c.provoke.clear(); }
        let _ = save(ctx, &c);
        ctx.live.send("pubsub.config", "", "updated");
    }
    true
}
