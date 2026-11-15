//! ai/config.yaml: the rules that shape what the simulated models answer, and the prices that make
//! the spend counter (per million tokens). The screen, the API and agents edit the same file.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use stuntman_core::{yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Config {
    /// First match wins.
    pub rules: Vec<Rule>,
    /// model (or prefix ending in *) → price per million tokens.
    pub prices: BTreeMap<String, Price>,
    /// What a call no rule takes is answered.
    pub default_text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Price {
    #[serde(rename = "in")]
    pub input: f64,
    pub out: f64,
    pub cached: f64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct When {
    /// openai | azure-openai | anthropic | gemini | openrouter | deepseek (empty: any)
    #[serde(skip_serializing_if = "String::is_empty")]
    pub provider: String,
    /// chat | responses | messages | embeddings | generate
    #[serde(skip_serializing_if = "String::is_empty")]
    pub api: String,
    /// The model, or a prefix with * (gpt-6.1*).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// The last user message contains this (any case).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub contains: String,
    /// The system prompt contains this.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub system_contains: String,
    /// The app offered this tool.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct ToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Reply {
    /// The text; {{ last_user }} is the last user message.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// Call a tool first; when the tool's result comes back, answer `text`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCall>,
    /// Answer this JSON (structured output).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json: Option<serde_json::Value>,
    /// Pad the answer to about this many tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    /// "length": stopped by max_tokens.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stop: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Rule {
    pub name: String,
    pub when: When,
    pub answer: Reply,
}

pub fn path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("ai/config.yaml")
}

pub fn load(ctx: &Ctx) -> Config {
    let mut c: Config = yamlfile::load(&path(ctx)).unwrap_or_default();
    for (k, p) in defaults() {
        c.prices.entry(k).or_insert(p);
    }
    c
}

pub fn save(ctx: &Ctx, c: &Config) -> Result<(), ApiError> {
    yamlfile::save(&path(ctx), c)
}

/// List prices (USD per million tokens) the counter starts from; edit them in Settings.
pub fn defaults() -> Vec<(String, Price)> {
    let p = |i, o, c| Price { input: i, out: o, cached: c };
    vec![
        ("gpt-*".into(), p(2.0, 8.0, 0.5)),
        ("gpt-*-mini".into(), p(0.4, 1.6, 0.1)),
        ("o*".into(), p(2.0, 8.0, 0.5)),
        ("text-embedding-*".into(), p(0.02, 0.0, 0.0)),
        ("claude-*".into(), p(3.0, 15.0, 0.3)),
        ("claude-*haiku*".into(), p(1.0, 5.0, 0.1)),
        ("gemini-*".into(), p(1.25, 10.0, 0.31)),
        ("deepseek-*".into(), p(0.27, 1.1, 0.07)),
    ]
}

pub fn glob(pat: &str, s: &str) -> bool {
    let parts: Vec<&str> = pat.split('*').collect();
    if parts.len() == 1 {
        return pat == s;
    }
    let mut rest = s;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            if !rest.starts_with(part) {
                return false;
            }
            rest = &rest[part.len()..];
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(k) = rest.find(part) {
            rest = &rest[k + part.len()..];
        } else {
            return false;
        }
    }
    true
}

/// The most specific price for a model (longest pattern that matches).
pub fn price(c: &Config, model: &str) -> Price {
    c.prices.iter().filter(|(k, _)| glob(k, model)).max_by_key(|(k, _)| k.len()).map(|(_, p)| p.clone()).unwrap_or_default()
}
