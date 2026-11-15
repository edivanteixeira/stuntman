//! The real Kinesis throughput limits, enforced on the wall clock (set_kinesis limits=off turns them
//! off). Provisioned: per shard 1 MiB/s or 1,000 records/s in, 2 MiB/s and 5 GetRecords/s out.
//! On-demand: writes per stream at the capacity a new on-demand stream gets (4 MiB/s, 4,000
//! records/s); reads per shard as provisioned. A read over 2 MiB is paid back over the next seconds,
//! so a 10 MiB GetRecords throttles the shard's reads for about 5 s, as Kinesis does.

use std::{collections::HashMap, sync::Mutex};

use crate::store::Stream;

pub const MIB: u64 = 1 << 20;

#[derive(Default)]
struct Win {
    sec: i64,
    recs: u64,
    bytes: u64,
    calls: u64,
}

// ponytail: one global map keyed env/stream/shard, never pruned; a deleted stream leaves a few bytes.
static W: Mutex<Option<HashMap<String, Win>>> = Mutex::new(None);

pub struct Limiter {
    pub env: String,
    pub on: bool,
    /// Wall clock, ms.
    pub now: i64,
}

impl Limiter {
    pub fn off() -> Self {
        Self { env: String::new(), on: false, now: 0 }
    }

    fn with<T>(&self, key: String, f: impl FnOnce(&mut Win, i64) -> T) -> T {
        let mut g = W.lock().unwrap();
        let w = g.get_or_insert_with(HashMap::new).entry(key).or_default();
        f(w, self.now.div_euclid(1000))
    }

    /// A write of `bytes` (data + partition key) to `shard`: false when it is over the limit.
    pub fn write(&self, s: &Stream, shard: &str, bytes: u64) -> bool {
        if !self.on { return true }
        let (key, recs, cap) = if s.mode == "ON_DEMAND" { (format!("{}/{}/*/w", self.env, s.name), 4_000, 4 * MIB) } else { (format!("{}/{}/{shard}/w", self.env, s.name), 1_000, MIB) };
        self.with(key, |w, sec| {
            if w.sec != sec { *w = Win { sec, ..Default::default() } }
            // The first record of a second always fits (a 1 MiB record plus its key).
            if w.recs + 1 > recs || (w.bytes > 0 && w.bytes + bytes > cap) { return false }
            w.recs += 1;
            w.bytes += bytes;
            true
        })
    }

    /// A GetRecords on `shard`: false when it is the 6th this second or the shard's reads are
    /// still paying back more than 2 MiB.
    pub fn read_call(&self, s: &Stream, shard: &str) -> bool {
        if !self.on { return true }
        self.with(format!("{}/{}/{shard}/r", self.env, s.name), |w, sec| {
            if w.sec != sec {
                w.bytes = w.bytes.saturating_sub(2 * MIB * (sec - w.sec).max(0) as u64);
                w.calls = 0;
                w.sec = sec;
            }
            if w.calls >= 5 || w.bytes >= 2 * MIB { return false }
            w.calls += 1;
            true
        })
    }

    pub fn read_bytes(&self, s: &Stream, shard: &str, bytes: u64) {
        if !self.on { return }
        self.with(format!("{}/{}/{shard}/r", self.env, s.name), |w, _| w.bytes += bytes);
    }
}

#[cfg(test)]
mod t {
    use super::*;

    fn stream(mode: &str) -> Stream {
        Stream { name: format!("lim-{mode}"), arn: String::new(), mode: mode.into(), retention: 24, created: 0, tags: Default::default(), next_seq: 1, next_shard: 0, shards: vec![],
            status: "ACTIVE".into(), ready_at: 0, encryption: "NONE".into(), key_id: None, metrics: vec![] }
    }

    #[test]
    fn per_shard_and_on_demand() {
        let s = stream("PROVISIONED");
        let l = Limiter { env: "t".into(), on: true, now: 5_000 };
        assert_eq!((0..1_500).filter(|_| l.write(&s, "shardId-000000000000", 10)).count(), 1_000);
        assert!(l.write(&s, "shardId-000000000001", 10), "another shard has its own budget");
        assert!(l.write(&s, "shardId-000000000002", MIB) && !l.write(&s, "shardId-000000000002", 1), "1 MiB/s");
        let next = Limiter { now: 6_000, ..l };
        assert!(next.write(&s, "shardId-000000000000", 10), "a new second");
        // Reads: 5 calls a second; 10 MiB read throttles for ~5 s.
        assert_eq!((0..8).filter(|_| next.read_call(&s, "shardId-000000000000")).count(), 5);
        next.read_bytes(&s, "shardId-000000000000", 10 * MIB);
        assert!(!Limiter { now: 9_000, env: "t".into(), on: true }.read_call(&s, "shardId-000000000000"));
        assert!(Limiter { now: 11_000, env: "t".into(), on: true }.read_call(&s, "shardId-000000000000"));
        let od = stream("ON_DEMAND");
        assert_eq!((0..5_000).filter(|i| next.write(&od, &format!("shardId-00000000000{}", i % 4), 10)).count(), 4_000);
        assert!(Limiter::off().write(&s, "shardId-000000000002", MIB));
    }
}
