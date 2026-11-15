//! Amazon Kinesis Data Streams (A21): the endpoint the AWS SDKs reach with
//! AWS_ENDPOINT_URL_KINESIS. Streams in provisioned and on-demand mode, shards with MD5 hash key
//! ranges, puts and iterators, resharding (split, merge, shard count) with parent and child shards,
//! retention on the environment clock, tags, enhanced fan-out consumers and SubscribeToShard;
//! the CREATING / UPDATING / DELETING lifecycle, the real per-shard throughput limits (on unless
//! set_kinesis limits=off), KMS encryption settings (records are not really encrypted), stream mode
//! changes, enhanced monitoring settings; provoked throughput, not-found, expired-iterator and KMS
//! errors, slow puts, partial failures.

pub mod api;
pub mod cbor;
pub mod config;
pub mod limits;
pub mod store;
pub mod tools;

use std::sync::atomic::{AtomicU16, Ordering};

use stuntman_core::{Module, Tool};

static PORT: AtomicU16 = AtomicU16::new(4567);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub struct Kinesis {
    pub port: u16,
}

impl Module for Kinesis {
    fn id(&self) -> &'static str {
        "kinesis"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        api::serve(envs, self.port);
    }
}
