//! Identity / SSO (I1–I5): an OpenID Connect provider that answers like Microsoft Entra ID (and Google,
//! Okta or any OIDC as flavours) with synthetic people, app registrations, a login page, MFA and
//! consent steps, real RS256 tokens, and the failures of both the sign-in and the provider itself.

pub mod clients;
pub mod config;
pub mod flow;
pub mod oidc;
pub mod page;
pub mod store;
pub mod token;
pub mod tools;

use stuntman_core::{Ctx, Module, Tool};

/// Changes to a provider file are read-modify-write: one at a time, so a rotation and a provocation
/// made a moment apart don't overwrite each other.
pub static WRITE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Identity;

impl Module for Identity {
    fn id(&self) -> &'static str {
        "identity"
    }
    fn migrations(&self) -> &'static str {
        store::SQL
    }
    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }
    fn extra_routes(&self) -> axum::Router<Ctx> {
        oidc::routes()
    }
}
