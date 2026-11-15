//! kinesis/config.yaml: the failure provoked on the Kinesis port (one at a time, as Pub/Sub's).

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use stuntman_core::{yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "" | one of PROVOKE.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provoke: String,
    /// Calls the provocation still answers; 0 = until cleared.
    pub times: u32,
    /// Only this stream; empty = every stream.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stream: String,
    /// slow_puts: how long a put waits.
    pub delay_ms: u64,
    /// The real per-shard throughput limits are enforced (limits.rs); false turns them off.
    pub limits: bool,
    /// How long CREATING, UPDATING and DELETING last (wall clock).
    pub transition_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self { provoke: String::new(), times: 0, stream: String::new(), delay_ms: 3000, limits: true, transition_ms: 1000 }
    }
}

impl Config {
    pub fn clear(&mut self) {
        self.provoke.clear();
        self.times = 0;
        self.stream.clear();
    }
}

pub const PROVOKE: [(&str, &str); 9] = [
    ("throughput_puts", "PutRecord and PutRecords answer ProvisionedThroughputExceededException (the SDKs retry)"),
    ("throughput_gets", "GetRecords answers ProvisionedThroughputExceededException"),
    ("not_found", "every call naming the stream answers ResourceNotFoundException"),
    ("expired_iterator", "GetRecords answers ExpiredIteratorException: get a new iterator"),
    ("kms_throttling", "puts and gets on an encrypted stream answer KMSThrottlingException"),
    ("kms_disabled", "puts to an encrypted stream answer KMSDisabledException"),
    ("slow_puts", "each put waits delay_ms before answering"),
    ("partial_failures", "PutRecords fails every second record (FailedRecordCount > 0)"),
    ("reshard", "now: the stream's widest open shard is split in two (children read after the parent)"),
];

fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("kinesis/config.yaml")
}

pub fn load(ctx: &Ctx) -> Config {
    yamlfile::load(&path(ctx)).unwrap_or_default()
}

pub fn save(ctx: &Ctx, c: &Config) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), c)
}

/// The provocation that applies to this call (one of `kinds`, on this stream), counted down.
pub fn take(ctx: &Ctx, stream: &str, kinds: &[&str]) -> Option<Config> {
    static LOCK: Mutex<()> = Mutex::new(());
    let _g = LOCK.lock().unwrap();
    let mut c = load(ctx);
    if c.provoke.is_empty() || !kinds.contains(&c.provoke.as_str()) || (!c.stream.is_empty() && c.stream != stream) {
        return None;
    }
    let hit = c.clone();
    if c.times > 0 {
        c.times -= 1;
        if c.times == 0 { c.clear() }
        let _ = save(ctx, &c);
        ctx.live.send("kinesis.config", "", "updated");
    }
    Some(hit)
}
