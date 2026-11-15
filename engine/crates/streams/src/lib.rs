//! Streams (A17): a Kafka broker and a Schema Registry for apps that exchange events with partners
//! (shipments, invoices, positions). Topics, consumer groups and their lag, Avro schemas; messages
//! produced as the partner would; rebalances, leaders missing, SASL refused, slow fetches.

pub mod avro;
pub mod broker;
pub mod config;
pub mod registry;
pub mod store;
pub mod tools;
pub mod wire;

use std::sync::atomic::{AtomicU16, Ordering};

use stuntman_core::{Module, Tool};

static KAFKA: AtomicU16 = AtomicU16::new(9092);
static REGISTRY: AtomicU16 = AtomicU16::new(8081);

pub fn kafka_port() -> u16 {
    KAFKA.load(Ordering::Relaxed)
}

pub fn registry_port() -> u16 {
    REGISTRY.load(Ordering::Relaxed)
}

pub struct Streams {
    pub kafka_port: u16,
    pub registry_port: u16,
}

impl Module for Streams {
    fn id(&self) -> &'static str {
        "streams"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        KAFKA.store(self.kafka_port, Ordering::Relaxed);
        REGISTRY.store(self.registry_port, Ordering::Relaxed);
        broker::serve(envs.clone(), self.kafka_port);
        registry::serve(envs, self.registry_port);
    }
}
