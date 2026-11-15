//! E-mail: the first Stuntman service module. SMTP and SendGrid API intakes, inboxes with rules and
//! steps, extractors, and SendGrid-style event webhooks back to the app.

pub mod config;
pub mod events;
pub mod imap;
pub mod inbox;
pub mod ingest;
pub mod lifecycle;
pub mod mailbox;
pub mod message;
pub mod pop3;
pub mod sendgrid;
pub mod smtp;
pub mod tools;

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
    routing::get,
    Router,
};
use stuntman_core::{ApiError, Ctx, Module, Tool};

pub struct Email {
    /// SMTP port for the first service created on an empty data folder.
    pub smtp_port: u16,
    pub imap_port: u16,
    pub pop3_port: u16,
}

impl Module for Email {
    fn id(&self) -> &'static str {
        "email"
    }

    fn migrations(&self) -> &'static str {
        include_str!("schema.sql")
    }

    fn tools(&self) -> Vec<Tool> {
        let mut t = tools::all();
        t.extend(mailbox::tools());
        t
    }

    fn listen(&self, envs: stuntman_core::env::Envs) {
        imap::serve(envs.clone(), self.imap_port);
        pop3::serve(envs, self.pop3_port);
    }

    fn extra_routes(&self) -> Router<Ctx> {
        sendgrid::routes()
            .route("/api/messages/{id}/raw", get(raw))
            .route("/api/messages/{id}/attachments/{index}", get(attachment))
            .route("/api/channels/email/extractors.yaml", get(extractors_yaml))
            .route("/api/services/{name}/yaml", get(service_yaml))
    }

    fn stop(&self, ctx: &Ctx) {
        let c = ctx.clone();
        tokio::spawn(async move { smtp::reconcile(&c).await });
    }

    fn start(&self, ctx: Ctx) {
        // Only the first environment gets the default service; the others start from a copy or empty.
        if ctx.envs.default_name() == *ctx.env {
            if let Err(e) = config::ensure_default(&ctx, self.smtp_port) {
                eprintln!("email: {}", e.message);
            }
        }
        tokio::spawn(async move {
            smtp::reconcile(&ctx).await;
            while !ctx.state.gone() {
                for s in config::list(&ctx).unwrap_or_default() {
                    let _ = message::sweep(&ctx, s.limits.keep_ms(), s.limits.max).await;
                }
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            }
        });
    }
}

async fn raw(State(ctx): State<Ctx>, Path(id): Path<String>) -> Result<impl IntoResponse, ApiError> {
    let raw = message::raw(&ctx, &id).await?;
    Ok(([(header::CONTENT_TYPE, "message/rfc822".to_string()), (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{id}.eml\""))], raw))
}

async fn attachment(State(ctx): State<Ctx>, Path((id, index)): Path<(String, String)>) -> Result<impl IntoResponse, ApiError> {
    let a = message::attachment(&ctx, &id, &index).await?;
    Ok(([(header::CONTENT_TYPE, a.content_type), (header::CONTENT_DISPOSITION, format!("inline; filename=\"{}\"", a.name.replace('"', "")))], a.data))
}

async fn extractors_yaml(State(ctx): State<Ctx>) -> Result<impl IntoResponse, ApiError> {
    let s = std::fs::read_to_string(config::extractors_file(&ctx)).unwrap_or_else(|_| serde_yaml::to_string(&config::Extractors::default()).unwrap());
    Ok(([(header::CONTENT_TYPE, "application/yaml")], s))
}

async fn service_yaml(State(ctx): State<Ctx>, Path(name): Path<String>) -> Result<impl IntoResponse, ApiError> {
    config::valid_name(&name)?;
    let s = std::fs::read_to_string(config::file(&ctx, &name)).map_err(|_| ApiError::not_found(format!("No e-mail service “{name}”")))?;
    Ok(([(header::CONTENT_TYPE, "application/yaml")], s))
}
