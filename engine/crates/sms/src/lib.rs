//! SMS (Zenvia API v2): the app sends to Stuntman as to api.zenvia.com (SM1), each number is a
//! simulated person with steps (SM2), and the service block holds access, senders, callbacks,
//! opt-out and limits (SM3). Status changes and replies reach the app as Zenvia webhooks.

pub mod config;
pub mod flow;
pub mod store;
pub mod tools;
pub mod zenvia;

use axum::{extract::{Path, State}, http::header, response::IntoResponse, routing::get, Router};
use stuntman_core::{Ctx, Module, Tool};

pub struct Sms;

impl Module for Sms {
    fn id(&self) -> &'static str {
        "sms"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        zenvia::routes().route("/api/sms/files/{name}", get(|State(ctx): State<Ctx>, Path(name): Path<String>| async move {
            let n = match name.as_str() { "service.yaml" | "recipients.yaml" => name, _ => return (axum::http::StatusCode::NOT_FOUND, "").into_response() };
            ([(header::CONTENT_TYPE, "application/yaml")], config::text(&ctx, &n)).into_response()
        }))
    }

    fn start(&self, ctx: Ctx) {
        flow::resume(&ctx);
    }
}
