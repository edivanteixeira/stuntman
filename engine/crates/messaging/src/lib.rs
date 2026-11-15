//! Messaging (M1–M3): Azure Service Bus (AMQP 1.0) and RabbitMQ (AMQP 0-9-1) on one AMQP port, one
//! vocabulary for both — namespace, entity (queue, topic, subscription), message. Messages with
//! their delivery history, locks, dead-letter, consumers, "next delivery", rules, namespaces as YAML.

pub mod amqp;
pub mod amqp1;
pub mod broker;
pub mod config;
pub mod mgmt;
pub mod rabbit;
pub mod rabbit_mgmt;
pub mod rules;
pub mod tools;

use std::sync::atomic::{AtomicU16, Ordering};

use stuntman_core::{Ctx, Module, Tool};

static PORT: AtomicU16 = AtomicU16::new(5673);

pub fn port() -> u16 {
    PORT.load(Ordering::Relaxed)
}

pub struct Messaging {
    pub port: u16,
    pub tls_port: u16,
}

static TLS_PORT: AtomicU16 = AtomicU16::new(5671);

pub fn tls_port() -> u16 {
    TLS_PORT.load(Ordering::Relaxed)
}

impl Module for Messaging {
    fn id(&self) -> &'static str {
        "messaging"
    }

    fn migrations(&self) -> &'static str {
        broker::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> axum::Router<Ctx> {
        tools::routes()
    }

    fn start(&self, ctx: Ctx) {
        tools::seed_if_empty(&ctx);
        // Connections do not survive a restart: consumers still marked receiving are gone.
        let c2 = ctx.clone();
        tokio::spawn(async move {
            let now = stuntman_core::util::now_ms();
            let _ = c2.db.call(move |c| c.execute("UPDATE msg_consumers SET until=?1, reason='Stuntman restarted' WHERE until IS NULL", [now])).await;
        });
        tokio::spawn(async move {
            loop {
                if ctx.state.gone() {
                    break;
                }
                broker::sweep(&ctx).await;
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            }
        });
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        PORT.store(self.port, Ordering::Relaxed);
        TLS_PORT.store(self.tls_port, Ordering::Relaxed);
        tokio::spawn(amqp::serve(envs.clone(), self.port));
        tokio::spawn(amqp::serve_tls(envs.clone(), self.tls_port));
        rabbit_mgmt::serve(envs, std::env::var("STUNTMAN_RABBITMQ_MGMT_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(15672));
    }

    fn reset(&self, ctx: &Ctx) {
        tools::seed_if_empty(ctx);
    }
}
