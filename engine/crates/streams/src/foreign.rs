//! Topics that live elsewhere: a Kafka client that logs in with SASL PLAIN as another service's
//! Kafka endpoint (Azure Event Hubs: user "$ConnectionString") reads and writes that service's
//! partitions through this broker. The service registers itself once at start.

use std::{future::Future, pin::Pin, sync::OnceLock};

use stuntman_core::Ctx;

use crate::wire::Rec;

pub type Fut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub trait Foreign: Send + Sync {
    /// Some(scope) when these credentials belong to the service (an Event Hubs namespace).
    fn login(&self, ctx: &Ctx, user: &str, password: &str) -> Option<String>;
    fn partitions(&self, ctx: &Ctx, scope: &str, topic: &str) -> Option<i32>;
    fn topics(&self, ctx: &Ctx, scope: &str) -> Vec<(String, i32)>;
    /// Appends; the base offset, or why not.
    fn append(&self, ctx: Ctx, scope: String, topic: String, part: i32, recs: Vec<Rec>, producer: String) -> Fut<Result<i64, String>>;
    /// (log start, high watermark).
    fn marks(&self, ctx: Ctx, scope: String, topic: String, part: i32) -> Fut<(i64, i64)>;
    fn read(&self, ctx: Ctx, scope: String, topic: String, part: i32, from: i64, max_bytes: usize) -> Fut<Vec<Rec>>;
}

static F: OnceLock<Box<dyn Foreign>> = OnceLock::new();

pub fn register(f: Box<dyn Foreign>) {
    let _ = F.set(f);
}

pub fn get() -> Option<&'static dyn Foreign> {
    F.get().map(|b| b.as_ref())
}
