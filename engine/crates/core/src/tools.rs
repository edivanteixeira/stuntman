//! One action, two doors. A [`Tool`] is an MCP tool and, through its [`Route`]s, an HTTP endpoint.
//! The HTTP adapter merges path params, query string and JSON body into one argument object, so the
//! screen, tests and agents always call the same code with the same names.

use std::{future::Future, pin::Pin, sync::Arc};

use axum::{http::StatusCode, response::IntoResponse, Json};
use serde_json::{json, Map, Value};

use crate::Ctx;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    /// Extra fields for the caller (e.g. what did arrive when a wait times out).
    pub detail: Value,
}

impl ApiError {
    pub fn new(status: u16, m: impl Into<String>) -> Self {
        Self { status, message: m.into(), detail: Value::Null }
    }
    pub fn bad(m: impl Into<String>) -> Self {
        Self::new(400, m)
    }
    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(404, m)
    }
    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(500, m)
    }
    pub fn with(mut self, detail: Value) -> Self {
        self.detail = detail;
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let code = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (code, Json(json!({"error": self.message, "detail": self.detail}))).into_response()
    }
}

pub type ToolResult = Result<Value, ApiError>;
type Handler = Arc<dyn Fn(Ctx, Args) -> Pin<Box<dyn Future<Output = ToolResult> + Send>> + Send + Sync>;

#[derive(Clone)]
pub struct Route {
    pub method: &'static str,
    /// axum path, e.g. "/messages/{id}". Mounted under /api.
    pub path: &'static str,
}

#[derive(Clone)]
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    /// (name, description, required). Becomes the MCP input schema; all values are JSON.
    pub params: Vec<(&'static str, &'static str, bool)>,
    pub routes: Vec<Route>,
    pub handler: Handler,
}

impl Tool {
    pub fn new<F, Fut>(name: &'static str, description: &'static str, f: F) -> Self
    where
        F: Fn(Ctx, Args) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ToolResult> + Send + 'static,
    {
        Self {
            name,
            description,
            params: vec![],
            routes: vec![],
            handler: Arc::new(move |c, a| Box::pin(f(c, a))),
        }
    }
    pub fn param(mut self, name: &'static str, desc: &'static str) -> Self {
        self.params.push((name, desc, false));
        self
    }
    pub fn req(mut self, name: &'static str, desc: &'static str) -> Self {
        self.params.push((name, desc, true));
        self
    }
    pub fn route(mut self, method: &'static str, path: &'static str) -> Self {
        self.routes.push(Route { method, path });
        self
    }
    pub fn schema(&self) -> Value {
        let mut props = Map::new();
        let mut required = vec![];
        for (n, d, r) in &self.params {
            props.insert(n.to_string(), json!({"description": d}));
            if *r {
                required.push(json!(n));
            }
        }
        json!({"type": "object", "properties": props, "required": required})
    }
    pub async fn call(&self, ctx: Ctx, args: Value) -> ToolResult {
        (self.handler)(ctx, Args::from(args)).await
    }
}

/// Arguments as a loose JSON object: query strings arrive as text, MCP as typed JSON.
#[derive(Clone, Debug, Default)]
pub struct Args(pub Map<String, Value>);

impl From<Value> for Args {
    fn from(v: Value) -> Self {
        let mut m = Map::new();
        if let Value::Object(o) = v {
            for (k, v) in o {
                m.insert(snake(&k), v);
            }
        }
        Args(m)
    }
}

/// camelCase (HTTP bodies) → snake_case (MCP and code).
pub fn snake(k: &str) -> String {
    let mut s = String::new();
    for c in k.chars() {
        if c.is_ascii_uppercase() {
            s.push('_');
            s.push(c.to_ascii_lowercase());
        } else {
            s.push(c);
        }
    }
    s
}

impl Args {
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null() && v.as_str() != Some(""))
    }
    pub fn str(&self, k: &str) -> Option<String> {
        self.get(k).map(|v| match v {
            Value::String(s) => s.clone(),
            o => o.to_string(),
        })
    }
    pub fn need(&self, k: &str) -> Result<String, ApiError> {
        self.str(k).ok_or_else(|| ApiError::bad(format!("“{k}” is required")))
    }
    pub fn u64(&self, k: &str) -> Option<u64> {
        self.get(k).and_then(|v| v.as_u64().or_else(|| v.as_str()?.trim().parse().ok()))
    }
    pub fn bool(&self, k: &str) -> Option<bool> {
        self.get(k).and_then(|v| v.as_bool().or_else(|| v.as_str().map(|s| s == "true" || s == "1")))
    }
    /// Durations as "30s", "500ms", "2h"; bare numbers are seconds.
    pub fn ms(&self, k: &str) -> Result<Option<u64>, ApiError> {
        match self.str(k) {
            None => Ok(None),
            Some(s) => crate::util::parse_ms(&s).map(Some).ok_or_else(|| ApiError::bad(format!("“{k}”: not a duration: {s}"))),
        }
    }
    /// A list given as JSON array or comma-separated text.
    pub fn list(&self, k: &str) -> Vec<String> {
        match self.get(k) {
            Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(String::from).unwrap_or(v.to_string())).collect(),
            Some(Value::String(s)) => s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
            _ => vec![],
        }
    }
}
