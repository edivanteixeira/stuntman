//! AI models (A12): the model APIs apps call, simulated. Point the SDK's base URL at Stuntman
//! (http://localhost:8025/openai/v1, …/anthropic, …/gemini, …/azure-openai); any key works and is
//! never sent anywhere. Rules shape the answers; failures are forced for the next calls; every call
//! keeps its prompt, answer, tokens and cost.

pub mod config;
pub mod model;
pub mod store;
pub mod tools;

use axum::Router;
use stuntman_core::{Ctx, Module, Tool};

pub struct Ai;

impl Module for Ai {
    fn id(&self) -> &'static str {
        "ai"
    }

    fn migrations(&self) -> &'static str {
        store::SQL
    }

    fn tools(&self) -> Vec<Tool> {
        tools::all()
    }

    fn extra_routes(&self) -> Router<Ctx> {
        model::routes()
    }
}
