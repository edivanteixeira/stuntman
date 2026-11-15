//! Product events (A13): the analytics an app sends — PostHog, Segment, Mixpanel, GA4 Measurement
//! Protocol — received and kept per user, so a test proves what the app tracked. A plan lists the
//! events (and properties) a flow must send; check_events fails a run when one is missing. Personal
//! data in a property (e-mail, CPF, card) is flagged.

pub mod intake;
pub mod plan;
pub mod store;
pub mod tools;

use axum::Router;
use stuntman_core::{Ctx, Module, Tool};

pub struct Events;

impl Module for Events {
    fn id(&self) -> &'static str {
        "events"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        intake::routes()
    }
}
