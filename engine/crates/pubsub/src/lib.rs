//! Google Cloud Pub/Sub: the emulator apps reach with PUBSUB_EMULATOR_HOST (plaintext gRPC, no
//! auth), speaking google.pubsub.v1 Publisher and Subscriber to the official clients (.NET, Python,
//! Go, Java, Node) and SchemaService. Topics with schemas and retention, pull, push, BigQuery and
//! Cloud Storage subscriptions with filters, ack deadlines, redelivery, ordering keys, exactly-once
//! delivery and dead-letter topics, snapshots and seek; messages published as another service would; provoked UNAVAILABLE,
//! DEADLINE_EXCEEDED, RESOURCE_EXHAUSTED, NOT_FOUND, slow publishes and duplicate deliveries.

pub mod config;
pub mod deliver;
pub mod filter;
pub mod grpc;
pub mod ingest;
pub mod schema;
pub mod udf;
pub mod store;
pub mod tools;

/// google.pubsub.v1, generated from the official googleapis protos (Apache-2.0).
#[allow(clippy::all, deprecated, rustdoc::all)]
pub mod v1 {
    include!("google.pubsub.v1.rs");
}

use std::sync::{
    atomic::{AtomicU16, Ordering},
    OnceLock,
};

use stuntman_core::{Module, Tool};
use tokio::sync::Notify;

static PORT: AtomicU16 = AtomicU16::new(8085);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

/// Wakes pulls waiting for messages (published, nacked, sought). ponytail: one bell for every
/// subscription and environment; waiters poll their own every 250 ms anyway.
pub fn bell() -> &'static Notify {
    static B: OnceLock<Notify> = OnceLock::new();
    B.get_or_init(Notify::new)
}

pub fn ring() {
    bell().notify_waiters();
}

pub struct PubSub {
    pub port: u16,
}

impl Module for PubSub {
    fn id(&self) -> &'static str {
        "pubsub"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        deliver::run(envs.clone());
        grpc::serve(envs, self.port);
    }
}
