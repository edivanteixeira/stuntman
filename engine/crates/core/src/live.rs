//! Live updates for the screens (server-sent events) and the list of who is connected.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use serde::Serialize;
use tokio::sync::broadcast;

#[derive(Clone, Debug, Serialize)]
pub struct Change {
    /// "email.message", "email.inbox", "email.service"…
    pub topic: String,
    pub id: String,
    /// "created", "updated", "deleted"
    pub what: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Client {
    pub name: String,
    /// "MCP" or "API"
    pub via: String,
    pub last_seen: i64,
}

#[derive(Clone)]
pub struct Live {
    tx: broadcast::Sender<Change>,
    clients: Arc<Mutex<BTreeMap<String, Client>>>,
}

impl Default for Live {
    fn default() -> Self {
        Self { tx: broadcast::channel(1024).0, clients: Default::default() }
    }
}

impl Live {
    pub fn send(&self, topic: &str, id: &str, what: &str) {
        let _ = self.tx.send(Change { topic: topic.into(), id: id.into(), what: what.into() });
    }
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.tx.subscribe()
    }
    pub fn seen(&self, name: &str, via: &str) {
        let c = Client { name: name.into(), via: via.into(), last_seen: crate::util::now_ms() };
        self.clients.lock().unwrap().insert(format!("{via}:{name}"), c);
    }
    /// Clients seen in the last 10 minutes.
    pub fn clients(&self) -> Vec<Client> {
        let cut = crate::util::now_ms() - 600_000;
        self.clients.lock().unwrap().values().filter(|c| c.last_seen > cut).cloned().collect()
    }
}
