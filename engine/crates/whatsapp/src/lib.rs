//! WhatsApp (Meta Cloud API): the app sends to Stuntman as to graph.facebook.com (W1), templates live
//! in YAML with a status Stuntman keeps instead of Meta's review (W2), each number is a simulated
//! person with steps (W3), and the number block holds access, webhook, tier and quality (W4).

pub mod config;
pub mod flow;
pub mod graph;
pub mod store;
pub mod tools;

use axum::{extract::{Path, State}, http::header, response::IntoResponse, routing::get, Router};
use stuntman_core::{util, Ctx, Module, Tool};

pub struct WhatsApp;

impl Module for WhatsApp {
    fn id(&self) -> &'static str {
        "whatsapp"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        graph::routes().route("/api/whatsapp/files/{name}", get(|State(ctx): State<Ctx>, Path(name): Path<String>| async move {
            let n = match name.as_str() { "templates.yaml" | "recipients.yaml" | "number.yaml" => name, _ => return (axum::http::StatusCode::NOT_FOUND, "").into_response() };
            ([(header::CONTENT_TYPE, "application/yaml")], config::text(&ctx, &n)).into_response()
        }))
    }

    fn start(&self, ctx: Ctx) {
        flow::resume(&ctx);
        // Templates the app created through the API approve by themselves after approve_after.
        tokio::spawn(async move {
            while !ctx.state.gone() {
                if let (Ok(n), Ok(mut all)) = (config::number(&ctx), config::templates(&ctx)) {
                    let wait = util::parse_ms(&n.approve_after).unwrap_or(300_000) as i64;
                    let mut done = vec![];
                    for t in all.iter_mut().filter(|t| t.status == "pending") {
                        if let Some(c) = t.created_at {
                            if util::now_ms() - c >= wait { t.status = "approved".into(); t.created_at = None; done.push(t.clone()); }
                        }
                    }
                    if !done.is_empty() && config::save_templates(&ctx, &all).is_ok() {
                        for t in done { flow::template_event(&ctx, &t, "approved", ""); ctx.live.send("whatsapp.templates", &t.name, "approved"); }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }
}
