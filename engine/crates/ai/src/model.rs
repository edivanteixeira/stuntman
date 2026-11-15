//! The model APIs apps call (A12): OpenAI (chat completions, responses, embeddings, models),
//! Azure OpenAI (deployments and v1), Anthropic (messages, count_tokens, streaming events), Gemini
//! (generateContent, streamGenerateContent, embedContent), and the OpenAI-compatible OpenRouter and
//! DeepSeek. Rules shape the answer; forced failures come first; every call keeps its prompt,
//! answer, tokens and cost.

use std::{collections::HashMap, sync::Mutex, time::Duration};

use axum::{
    body::{Body, Bytes},
    extract::{Path, RawQuery, State},
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use serde_json::{json, Value};
use sha2::Digest;
use stuntman_core::{util, Ctx};

use crate::{config, store};

pub const PROVIDERS: [&str; 6] = ["openai", "azure-openai", "anthropic", "gemini", "openrouter", "deepseek"];
pub const FAILURES: [(&str, &str); 8] = [("429", "rate limit, Retry-After"), ("500", "server error"), ("529", "overloaded"), ("filter", "content filtered"),
    ("length", "cut by max_tokens"), ("bad_json", "tool arguments broken"), ("slow", "40 ms per token"), ("cut", "the stream stops halfway")];

pub fn routes() -> Router<Ctx> {
    let mut r = Router::new();
    for p in PROVIDERS {
        r = r.route(&format!("/{p}/{{*rest}}"), any(move |State(ctx): State<Ctx>, Path(rest): Path<String>, method: Method, headers: HeaderMap, RawQuery(q): RawQuery, body: Bytes| async move {
            handle(ctx, p, rest, method, headers, q.unwrap_or_default(), body).await
        }));
    }
    r
}

/// What the app asked, whatever the API.
#[derive(Default, Debug)]
pub struct Prompt {
    pub system: String,
    pub last_user: String,
    pub all: String,
    pub tool_result: bool,
    pub tools: Vec<String>,
    pub model: String,
    pub max_tokens: Option<u64>,
    pub stream: bool,
    pub inputs: Vec<String>,
}

fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(|p| match p {
            Value::String(s) => s.clone(),
            o => o.get("text").and_then(|t| t.as_str()).map(String::from).or_else(|| o.get("content").map(text_of)).unwrap_or_default(),
        }).collect::<Vec<_>>().join("\n"),
        Value::Object(o) => o.get("text").and_then(|t| t.as_str()).map(String::from).unwrap_or_default(),
        _ => String::new(),
    }
}

pub fn prompt(api: &str, b: &Value, model_hint: &str) -> Prompt {
    let mut p = Prompt { model: b["model"].as_str().unwrap_or(model_hint).to_string(), stream: b["stream"].as_bool().unwrap_or(false), ..Default::default() };
    let mut all = vec![];
    match api {
        "chat" => {
            p.max_tokens = b["max_completion_tokens"].as_u64().or(b["max_tokens"].as_u64());
            for m in b["messages"].as_array().cloned().unwrap_or_default() {
                let t = text_of(&m["content"]);
                match m["role"].as_str().unwrap_or("") {
                    "system" | "developer" => p.system.push_str(&t),
                    "user" => p.last_user = t.clone(),
                    "tool" | "function" => p.tool_result = true,
                    _ => {}
                }
                all.push(t);
            }
            if let Some(Value::String(r)) = b["messages"].as_array().and_then(|a| a.last()).map(|m| m["role"].clone()) {
                p.tool_result = r == "tool" || r == "function";
            }
            p.tools = b["tools"].as_array().map(|t| t.iter().filter_map(|x| x.pointer("/function/name").or(x.get("name")).and_then(|n| n.as_str()).map(String::from)).collect()).unwrap_or_default();
        }
        "responses" => {
            p.max_tokens = b["max_output_tokens"].as_u64();
            p.system = b["instructions"].as_str().unwrap_or("").to_string();
            match &b["input"] {
                Value::String(s) => {
                    p.last_user = s.clone();
                    all.push(s.clone());
                }
                Value::Array(items) => {
                    for it in items {
                        let t = text_of(&it["content"]);
                        match (it["type"].as_str().unwrap_or("message"), it["role"].as_str().unwrap_or("")) {
                            ("function_call_output", _) => p.tool_result = true,
                            (_, "system" | "developer") => p.system.push_str(&t),
                            (_, "user") => {
                                p.last_user = t.clone();
                                p.tool_result = false;
                            }
                            _ => {}
                        }
                        all.push(t);
                    }
                }
                _ => {}
            }
            p.tools = b["tools"].as_array().map(|t| t.iter().filter_map(|x| x["name"].as_str().map(String::from)).collect()).unwrap_or_default();
        }
        "messages" | "count" => {
            p.max_tokens = b["max_tokens"].as_u64();
            p.system = text_of(&b["system"]);
            for m in b["messages"].as_array().cloned().unwrap_or_default() {
                let t = text_of(&m["content"]);
                if m["role"] == "user" {
                    let has_result = m["content"].as_array().map(|c| c.iter().any(|x| x["type"] == "tool_result")).unwrap_or(false);
                    p.tool_result = has_result;
                    if !has_result {
                        p.last_user = t.clone();
                    }
                }
                all.push(t);
            }
            p.tools = b["tools"].as_array().map(|t| t.iter().filter_map(|x| x["name"].as_str().map(String::from)).collect()).unwrap_or_default();
        }
        "generate" => {
            p.max_tokens = b.pointer("/generationConfig/maxOutputTokens").and_then(|v| v.as_u64());
            p.system = b.pointer("/systemInstruction/parts").map(text_of).unwrap_or_default();
            for c in b["contents"].as_array().cloned().unwrap_or_default() {
                let parts = c["parts"].as_array().cloned().unwrap_or_default();
                let t = text_of(&c["parts"]);
                if c["role"].as_str().unwrap_or("user") == "user" {
                    let has_result = parts.iter().any(|x| x.get("functionResponse").is_some());
                    p.tool_result = has_result;
                    if !has_result {
                        p.last_user = t.clone();
                    }
                }
                all.push(t);
            }
            p.tools = b["tools"].as_array().map(|t| t.iter().flat_map(|x| x["functionDeclarations"].as_array().cloned().unwrap_or_default()).filter_map(|f| f["name"].as_str().map(String::from)).collect()).unwrap_or_default();
        }
        "embeddings" => {
            p.inputs = match (&b["input"], &b["content"], &b["requests"]) {
                (Value::String(s), _, _) => vec![s.clone()],
                (Value::Array(a), _, _) => a.iter().map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())).collect(),
                (_, c, _) if !c.is_null() => vec![text_of(&c["parts"])],
                (_, _, Value::Array(r)) => r.iter().map(|x| text_of(&x["content"]["parts"])).collect(),
                _ => vec![],
            };
            all = p.inputs.clone();
        }
        _ => {}
    }
    p.all = all.join("\n");
    p
}

/// About 4 characters per token, as the providers count English.
pub fn tokens(s: &str) -> u64 {
    (s.chars().count() as u64).div_ceil(4).max(if s.is_empty() { 0 } else { 1 })
}

/// The same text always gives the same unit vector, so search tests repeat.
pub fn embedding(text: &str, dims: usize) -> Vec<f32> {
    let mut seed = sha2::Sha256::digest(text.as_bytes()).to_vec();
    let mut v = Vec::with_capacity(dims);
    while v.len() < dims {
        seed = sha2::Sha256::digest(&seed).to_vec();
        for c in seed.chunks(2) {
            if v.len() < dims {
                v.push((u16::from_le_bytes([c[0], c[1]]) as f32 / 32767.5) - 1.0);
            }
        }
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter().map(|x| x / n).collect()
}

/// What the simulated model decided.
#[derive(Debug, Clone, Default)]
pub struct Out {
    pub text: String,
    pub tool: Option<(String, String)>,
    /// "" | length | filter
    pub stop: String,
    pub why: String,
    pub fail: Option<String>,
    pub slow: bool,
    pub cut: bool,
}

fn pad(text: &str, n: u64) -> String {
    let mut t = text.to_string();
    let words = ["the", "simulated", "model", "keeps", "writing", "so", "the", "answer", "has", "the", "length", "you", "asked", "for."];
    let mut i = 0;
    while tokens(&t) < n {
        if !t.is_empty() {
            t.push(' ');
        }
        t.push_str(words[i % words.len()]);
        i += 1;
    }
    t
}

pub fn decide(cfg: &config::Config, provider: &str, api: &str, p: &Prompt) -> Out {
    let lu = p.last_user.to_lowercase();
    for r in &cfg.rules {
        let w = &r.when;
        let ok = (w.provider.is_empty() || w.provider == provider)
            && (w.api.is_empty() || w.api == api)
            && (w.model.is_empty() || config::glob(&w.model, &p.model))
            && (w.contains.is_empty() || lu.contains(&w.contains.to_lowercase()))
            && (w.system_contains.is_empty() || p.system.to_lowercase().contains(&w.system_contains.to_lowercase()))
            && (w.tool.is_empty() || p.tools.iter().any(|t| t == &w.tool));
        if !ok {
            continue;
        }
        let a = &r.answer;
        let mut o = Out { why: format!("rule: {}", r.name), stop: a.stop.clone(), ..Default::default() };
        if let (Some(tc), false) = (&a.tool_call, p.tool_result) {
            o.tool = Some((tc.name.clone(), tc.arguments.to_string()));
            o.why = format!("rule: {} · tool call", r.name);
            return o;
        }
        o.text = match &a.json {
            Some(j) => j.to_string(),
            None if a.text.is_empty() && a.tool_call.is_some() => "Done.".into(),
            None => a.text.replace("{{ last_user }}", &p.last_user).replace("{{last_user}}", &p.last_user),
        };
        if let Some(n) = a.tokens {
            o.text = pad(&o.text, n);
        }
        return o;
    }
    let text = if cfg.default_text.is_empty() { "This is a simulated answer from Stuntman.".to_string() } else { cfg.default_text.clone() };
    Out { text, why: "no rule: default answer".into(), ..Default::default() }
}

/// Prompts seen in this process: a prompt whose start was sent before is mostly cached.
fn cached_tokens(p: &Prompt, tin: u64) -> u64 {
    static SEEN: Mutex<Option<HashMap<String, ()>>> = Mutex::new(None);
    let head: String = format!("{}{}", p.system, p.all).chars().take(4096).collect();
    if tokens(&head) < 256 {
        return 0;
    }
    let key = hex(&sha2::Sha256::digest(head.as_bytes()));
    let mut g = SEEN.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::new);
    if m.insert(key, ()).is_some() { (tin * 4 / 5).min(tokens(&head)) } else { 0 }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn rid(prefix: &str) -> String {
    format!("{prefix}{}", util::uuid().replace('-', "")[..24].to_string())
}

fn err_json(provider: &str, status: u16, kind: &str, msg: &str) -> Value {
    match provider {
        "anthropic" => json!({"type": "error", "error": {"type": kind, "message": msg}, "request_id": rid("req_")}),
        "gemini" => json!({"error": {"code": status, "message": msg, "status": kind}}),
        _ => json!({"error": {"message": msg, "type": kind, "param": null, "code": kind}}),
    }
}

fn failure(provider: &str, kind: &str) -> (u16, Value, Vec<(&'static str, String)>) {
    match (kind, provider) {
        ("429", "anthropic") => (429, err_json(provider, 429, "rate_limit_error", "Number of request tokens has exceeded your per-minute rate limit"), vec![("retry-after", "2".into()), ("anthropic-ratelimit-requests-remaining", "0".into())]),
        ("429", "gemini") => (429, err_json(provider, 429, "RESOURCE_EXHAUSTED", "Resource has been exhausted (e.g. check quota)."), vec![]),
        ("429", _) => (429, err_json(provider, 429, "rate_limit_exceeded", "Rate limit reached for requests. Please try again in 2s."), vec![("retry-after", "2".into()), ("x-ratelimit-remaining-requests", "0".into())]),
        ("529", "anthropic") => (529, err_json(provider, 529, "overloaded_error", "Overloaded"), vec![]),
        ("529", "gemini") => (503, err_json(provider, 503, "UNAVAILABLE", "The model is overloaded. Please try again later."), vec![]),
        ("529", _) => (503, err_json(provider, 503, "server_overloaded", "The engine is currently overloaded, please try again later"), vec![]),
        ("filter", "azure-openai") => (400, json!({"error": {"message": "The response was filtered due to the prompt triggering Azure OpenAI's content management policy.", "type": null, "param": "prompt", "code": "content_filter", "status": 400,
            "innererror": {"code": "ResponsibleAIPolicyViolation", "content_filter_result": {"hate": {"filtered": true, "severity": "high"}}}}}), vec![]),
        ("500", "anthropic") => (500, err_json(provider, 500, "api_error", "Internal server error"), vec![]),
        ("500", "gemini") => (500, err_json(provider, 500, "INTERNAL", "An internal error has occurred."), vec![]),
        _ => (500, err_json(provider, 500, "server_error", "The server had an error while processing your request. Sorry about that!"), vec![]),
    }
}

fn http(status: u16, body: Value, headers: Vec<(&'static str, String)>) -> Response {
    let mut r = (StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), axum::Json(body)).into_response();
    for (k, v) in headers {
        if let Ok(v) = v.parse() {
            r.headers_mut().insert(k, v);
        }
    }
    r
}

/// SSE parts sent one after the other; cut breaks the connection after half of them.
fn sse(parts: Vec<String>, slow: bool, cut: bool, json_lines: bool) -> Response {
    let mut parts = parts;
    if cut {
        parts.truncate((parts.len() / 2).max(1));
    }
    let wait = if slow { 40 } else { 8 };
    let st = futures::stream::unfold((parts.into_iter(), false), move |(mut it, done)| async move {
        if done {
            return None;
        }
        match it.next() {
            Some(p) => {
                tokio::time::sleep(Duration::from_millis(wait)).await;
                Some((Ok::<_, std::io::Error>(Bytes::from(p)), (it, false)))
            }
            None if cut => {
                tokio::time::sleep(Duration::from_millis(150)).await;
                Some((Err(std::io::Error::other("stream cut on purpose")), (it, true)))
            }
            None => None,
        }
    });
    let mut r = Response::new(Body::from_stream(st));
    r.headers_mut().insert("content-type", if json_lines { "application/json" } else { "text/event-stream" }.parse().unwrap());
    r.headers_mut().insert("cache-control", "no-cache".parse().unwrap());
    r
}

fn chunks(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_inclusive(' ').collect();
    words.chunks(3).map(|c| c.concat()).collect()
}

fn data(v: &Value) -> String {
    format!("data: {v}\n\n")
}

fn event(name: &str, v: &Value) -> String {
    format!("event: {name}\ndata: {v}\n\n")
}

#[allow(clippy::too_many_arguments)]
async fn handle(ctx: Ctx, provider: &'static str, rest: String, method: Method, headers: HeaderMap, query: String, body: Bytes) -> Response {
    let t0 = std::time::Instant::now();
    let rest = rest.trim_matches('/').to_string();
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let caller = { let ua = h("user-agent"); ua.split([' ', '/']).next().filter(|s| !s.is_empty()).unwrap_or("app").to_string() };
    ctx.live.seen(&caller, "AI models");
    // Which API.
    let (api, model_hint) = if provider == "gemini" {
        let seg = rest.rsplit('/').next().unwrap_or("");
        let (m, action) = seg.split_once(':').unwrap_or((seg, ""));
        (match action { "generateContent" | "streamGenerateContent" => "generate", "embedContent" | "batchEmbedContents" => "embeddings", "countTokens" => "count", _ if method == Method::GET => "models", _ => "" }, m.to_string())
    } else if rest.ends_with("chat/completions") {
        ("chat", rest.split("deployments/").nth(1).and_then(|x| x.split('/').next()).unwrap_or("").to_string())
    } else if rest.ends_with("responses") {
        ("responses", String::new())
    } else if rest.ends_with("embeddings") {
        ("embeddings", rest.split("deployments/").nth(1).and_then(|x| x.split('/').next()).unwrap_or("").to_string())
    } else if rest.ends_with("messages/count_tokens") {
        ("count", String::new())
    } else if rest.ends_with("messages") {
        ("messages", String::new())
    } else if rest.ends_with("models") || rest.contains("models/") && method == Method::GET {
        ("models", String::new())
    } else {
        ("", String::new())
    };
    if api.is_empty() {
        return http(404, err_json(provider, 404, "not_found", &format!("Stuntman: {provider} has no API at /{rest}")), vec![]);
    }
    if api == "models" {
        const CLAUDE: [&str; 3] = ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"];
        const GEMINI: [&str; 3] = ["gemini-3-pro", "gemini-3-flash", "text-embedding-005"];
        const OPENAI: [&str; 4] = ["gpt-6.1", "gpt-6.1-mini", "text-embedding-3-small", "text-embedding-3-large"];
        let list = match provider {
            "anthropic" => json!({"data": CLAUDE.into_iter().map(|m| json!({"type": "model", "id": m, "display_name": m, "created_at": "2026-01-01T00:00:00Z"})).collect::<Vec<_>>(), "has_more": false}),
            "gemini" => json!({"models": GEMINI.into_iter().map(|m| json!({"name": format!("models/{m}"), "displayName": m})).collect::<Vec<_>>()}),
            _ => json!({"object": "list", "data": OPENAI.into_iter().map(|m| json!({"id": m, "object": "model", "created": 1767225600, "owned_by": "stuntman"})).collect::<Vec<_>>()}),
        };
        return http(200, list, vec![]);
    }
    // Any key; none is refused, as the providers do.
    let key = match provider {
        "anthropic" => h("x-api-key") + &h("authorization"),
        "gemini" => h("x-goog-api-key") + &h("authorization") + if query.contains("key=") { "q" } else { "" },
        "azure-openai" => h("api-key") + &h("authorization"),
        _ => h("authorization"),
    };
    if key.trim().is_empty() {
        let (kind, msg) = match provider { "anthropic" => ("authentication_error", "x-api-key header is required"), "gemini" => ("UNAUTHENTICATED", "Method doesn't allow unregistered callers."), _ => ("invalid_api_key", "You didn't provide an API key.") };
        return http(401, err_json(provider, 401, kind, msg), vec![]);
    }
    let b: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let stream_q = query.contains("alt=sse") || rest.contains(":streamGenerateContent");
    let mut p = prompt(api, &b, &model_hint);
    if p.model.is_empty() {
        p.model = model_hint.clone();
    }
    if stream_q {
        p.stream = true;
    }
    let cfg = config::load(&ctx);
    let tin = tokens(&format!("{}{}", p.system, p.all)) + 8;
    if api == "count" {
        let v = if provider == "gemini" { json!({"totalTokens": tin}) } else { json!({"input_tokens": tin}) };
        return http(200, v, vec![]);
    }
    let prompt_json = json!({"system": p.system, "lastUser": p.last_user, "tools": p.tools, "toolResult": p.tool_result, "maxTokens": p.max_tokens, "inputs": p.inputs.len()});
    let id = rid(match provider { "anthropic" => "msg_", _ if api == "responses" => "resp_", _ => "chatcmpl-" });

    // Forced failures first.
    let mut out = if api == "embeddings" { Out { why: "deterministic".into(), ..Default::default() } } else { decide(&cfg, provider, api, &p) };
    if let Ok(Some((kind, by))) = store::take_force(&ctx, provider).await {
        out.why = format!("forced by {} · {kind}", by.trim_start_matches("api:").trim_start_matches("mcp:"));
        match kind.as_str() {
            "slow" => out.slow = true,
            "cut" => out.cut = true,
            "length" => out.stop = "length".into(),
            "filter" if provider != "azure-openai" => out.stop = "filter".into(),
            "bad_json" => out.tool = Some((p.tools.first().cloned().unwrap_or("lookup".into()), "{\"order_id\": \"A-10".into())),
            k => out.fail = Some(k.to_string()),
        }
    }
    let price = config::price(&cfg, &p.model);
    let finish = |status: u16, tout: u64, cached: u64, answer: String, response: String, stream: bool, why: String| {
        let (ctx, provider, p_model, prompt_json, b, id, caller) = (ctx.clone(), provider.to_string(), p.model.clone(), prompt_json.clone(), b.clone(), id.clone(), caller.clone());
        let cost = ((tin.saturating_sub(cached)) as f64 * price.input + cached as f64 * price.cached + tout as f64 * price.out) / 1e6;
        let ms = t0.elapsed().as_millis() as i64;
        async move {
            let _ = store::record(&ctx, store::Call { id: id.clone(), provider: provider.clone(), api: api.to_string(), model: p_model.clone(), status, why: why.clone(), tokens_in: tin, tokens_out: tout, cached, cost, stream, ms, caller: caller.clone(), prompt: prompt_json, answer: answer.clone(), request: b, response }).await;
            let label = format!("{provider} {api} · {p_model} · {}", if status >= 400 { format!("{status}") } else { format!("{tin} → {tout} tokens") });
            let _ = stuntman_core::trips::record(&ctx, stuntman_core::trips::Trip { id: &id, module: "ai", label: &label, status: &status.to_string(), trace_id: None, link: &format!("/ai?call={id}"), host: &provider,
                actor: &caller, outcome: if why.starts_with("forced") { "provoked" } else if status >= 400 { "failed" } else { "ok" }, detail: &why }).await;
        }
    };
    if let Some(kind) = &out.fail {
        let (st, body, hs) = failure(provider, kind);
        finish(st, 0, 0, String::new(), body.to_string(), p.stream, out.why.clone()).await;
        return http(st, body, hs);
    }
    if out.stop == "length" {
        let n = p.max_tokens.unwrap_or(16).min(4096);
        out.text = pad(&out.text, n);
        let w: Vec<&str> = out.text.split(' ').collect();
        out.text = w.iter().take((n as usize * 3 / 4).max(1)).cloned().collect::<Vec<_>>().join(" ");
    }
    if out.stop == "filter" {
        out.text.clear();
    }

    // Embeddings.
    if api == "embeddings" {
        let dims = b["dimensions"].as_u64().or(b["outputDimensionality"].as_u64()).unwrap_or(if p.model.contains("large") { 3072 } else if provider == "gemini" { 768 } else { 1536 }) as usize;
        let vecs: Vec<Vec<f32>> = p.inputs.iter().map(|t| embedding(t, dims)).collect();
        let b64 = b["encoding_format"] == "base64";
        let v = if provider == "gemini" {
            if vecs.len() == 1 && b.get("content").is_some() { json!({"embedding": {"values": vecs[0]}}) } else { json!({"embeddings": vecs.iter().map(|x| json!({"values": x})).collect::<Vec<_>>()}) }
        } else {
            json!({"object": "list", "model": p.model, "usage": {"prompt_tokens": tin, "total_tokens": tin},
                "data": vecs.iter().enumerate().map(|(i, x)| json!({"object": "embedding", "index": i, "embedding": if b64 {
                    use base64::Engine;
                    json!(base64::engine::general_purpose::STANDARD.encode(x.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>()))
                } else { json!(x) }})).collect::<Vec<_>>()})
        };
        finish(200, 0, 0, format!("{} vectors × {dims}", vecs.len()), String::new(), false, out.why.clone()).await;
        return http(200, v, vec![]);
    }

    let cached = cached_tokens(&p, tin);
    let tool_args = out.tool.as_ref().map(|t| t.1.clone()).unwrap_or_default();
    let tout = tokens(&out.text) + tokens(&tool_args) + if out.tool.is_some() { 8 } else { 0 };
    let answer_text = match &out.tool { Some((n, a)) => format!("tool {n} {a}{}", if out.text.is_empty() { String::new() } else { format!(" · {}", out.text) }), None => out.text.clone() };
    let created = util::now_ms() / 1000;
    let model = p.model.clone();
    let response: Response;
    let mut raw = String::new();
    match (provider, api) {
        ("anthropic", _) => {
            let stop = match (&out.tool, out.stop.as_str()) { (Some(_), _) => "tool_use", (_, "length") => "max_tokens", (_, "filter") => "refusal", _ => "end_turn" };
            let tool_id = rid("toolu_");
            let mut content = vec![];
            if !out.text.is_empty() {
                content.push(json!({"type": "text", "text": out.text}));
            }
            if let Some((n, a)) = &out.tool {
                content.push(json!({"type": "tool_use", "id": tool_id, "name": n, "input": serde_json::from_str::<Value>(a).unwrap_or(json!(a))}));
            }
            let usage = json!({"input_tokens": tin - cached, "cache_read_input_tokens": cached, "cache_creation_input_tokens": 0, "output_tokens": tout});
            if p.stream {
                let mut parts = vec![event("message_start", &json!({"type": "message_start", "message": {"id": id, "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "usage": {"input_tokens": tin - cached, "output_tokens": 1}}}))];
                let mut idx = 0;
                if !out.text.is_empty() {
                    parts.push(event("content_block_start", &json!({"type": "content_block_start", "index": idx, "content_block": {"type": "text", "text": ""}})));
                    for c in chunks(&out.text) {
                        parts.push(event("content_block_delta", &json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": c}})));
                    }
                    parts.push(event("content_block_stop", &json!({"type": "content_block_stop", "index": idx})));
                    idx += 1;
                }
                if let Some((n, a)) = &out.tool {
                    parts.push(event("content_block_start", &json!({"type": "content_block_start", "index": idx, "content_block": {"type": "tool_use", "id": tool_id, "name": n, "input": {}}})));
                    for c in a.as_bytes().chunks(12) {
                        parts.push(event("content_block_delta", &json!({"type": "content_block_delta", "index": idx, "delta": {"type": "input_json_delta", "partial_json": String::from_utf8_lossy(c)}})));
                    }
                    parts.push(event("content_block_stop", &json!({"type": "content_block_stop", "index": idx})));
                }
                parts.push(event("message_delta", &json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": tout}})));
                parts.push(event("message_stop", &json!({"type": "message_stop"})));
                raw = parts.concat();
                response = sse(parts, out.slow, out.cut, false);
            } else {
                let v = json!({"id": id, "type": "message", "role": "assistant", "model": model, "content": content, "stop_reason": stop, "stop_sequence": null, "usage": usage});
                raw = v.to_string();
                response = http(200, v, vec![("request-id", rid("req_"))]);
            }
        }
        ("gemini", _) => {
            let finish_reason = match out.stop.as_str() { "length" => "MAX_TOKENS", "filter" => "SAFETY", _ => "STOP" };
            let mut parts_json = vec![];
            if !out.text.is_empty() {
                parts_json.push(json!({"text": out.text}));
            }
            if let Some((n, a)) = &out.tool {
                parts_json.push(json!({"functionCall": {"name": n, "args": serde_json::from_str::<Value>(a).unwrap_or(json!({"raw": a}))}}));
            }
            let usage = json!({"promptTokenCount": tin, "candidatesTokenCount": tout, "totalTokenCount": tin + tout, "cachedContentTokenCount": cached});
            if p.stream {
                let mut parts: Vec<String> = chunks(&out.text).into_iter().map(|c| data(&json!({"candidates": [{"content": {"parts": [{"text": c}], "role": "model"}, "index": 0}], "modelVersion": model}))).collect();
                if let Some((n, a)) = &out.tool {
                    parts.push(data(&json!({"candidates": [{"content": {"parts": [{"functionCall": {"name": n, "args": serde_json::from_str::<Value>(a).unwrap_or(json!({}))}}], "role": "model"}, "index": 0}]})));
                }
                parts.push(data(&json!({"candidates": [{"content": {"parts": [{"text": ""}], "role": "model"}, "finishReason": finish_reason, "index": 0}], "usageMetadata": usage, "modelVersion": model})));
                raw = parts.concat();
                response = sse(parts, out.slow, out.cut, false);
            } else {
                let v = json!({"candidates": [{"content": {"parts": parts_json, "role": "model"}, "finishReason": finish_reason, "index": 0}], "usageMetadata": usage, "modelVersion": model, "responseId": id});
                raw = v.to_string();
                response = http(200, v, vec![]);
            }
        }
        (_, "responses") => {
            let call_id = rid("call_");
            let mut output = vec![];
            if let Some((n, a)) = &out.tool {
                output.push(json!({"type": "function_call", "id": rid("fc_"), "call_id": call_id, "name": n, "arguments": a, "status": "completed"}));
            }
            let msg_id = rid("msg_");
            if !out.text.is_empty() || out.tool.is_none() {
                output.push(json!({"type": "message", "id": msg_id, "role": "assistant", "status": "completed", "content": [{"type": "output_text", "text": out.text, "annotations": []}]}));
            }
            let status = if out.stop == "length" { "incomplete" } else { "completed" };
            let usage = json!({"input_tokens": tin, "input_tokens_details": {"cached_tokens": cached}, "output_tokens": tout, "output_tokens_details": {"reasoning_tokens": 0}, "total_tokens": tin + tout});
            let full = json!({"id": id, "object": "response", "created_at": created, "status": status, "model": model, "output": output, "usage": usage,
                "incomplete_details": if status == "incomplete" { json!({"reason": "max_output_tokens"}) } else { Value::Null }, "output_text": out.text});
            if p.stream {
                let mut seq = 0;
                let mut ev = |name: &str, mut v: Value| { v["type"] = json!(name); v["sequence_number"] = json!(seq); seq += 1; event(name, &v) };
                let mut parts = vec![ev("response.created", json!({"response": {"id": id, "object": "response", "status": "in_progress", "model": model, "output": []}}))];
                let mut oi = 0;
                if let Some((n, a)) = &out.tool {
                    parts.push(ev("response.output_item.added", json!({"output_index": oi, "item": {"type": "function_call", "call_id": call_id, "name": n, "arguments": ""}})));
                    for c in a.as_bytes().chunks(12) {
                        parts.push(ev("response.function_call_arguments.delta", json!({"output_index": oi, "delta": String::from_utf8_lossy(c)})));
                    }
                    parts.push(ev("response.function_call_arguments.done", json!({"output_index": oi, "arguments": a})));
                    parts.push(ev("response.output_item.done", json!({"output_index": oi, "item": output[0]})));
                    oi += 1;
                }
                if !out.text.is_empty() {
                    parts.push(ev("response.output_item.added", json!({"output_index": oi, "item": {"type": "message", "id": msg_id, "role": "assistant", "content": []}})));
                    for c in chunks(&out.text) {
                        parts.push(ev("response.output_text.delta", json!({"item_id": msg_id, "output_index": oi, "content_index": 0, "delta": c})));
                    }
                    parts.push(ev("response.output_text.done", json!({"item_id": msg_id, "output_index": oi, "content_index": 0, "text": out.text})));
                    parts.push(ev("response.output_item.done", json!({"output_index": oi, "item": output.last().cloned().unwrap_or(Value::Null)})));
                }
                parts.push(ev(if status == "incomplete" { "response.incomplete" } else { "response.completed" }, json!({"response": full})));
                raw = parts.concat();
                response = sse(parts, out.slow, out.cut, false);
            } else {
                raw = full.to_string();
                response = http(200, full, vec![]);
            }
        }
        _ => {
            // Chat completions: OpenAI, Azure OpenAI, OpenRouter, DeepSeek.
            let finish_reason = match (&out.tool, out.stop.as_str()) { (Some(_), _) => "tool_calls", (_, "length") => "length", (_, "filter") => "content_filter", _ => "stop" };
            let call_id = rid("call_");
            let tool_calls = out.tool.as_ref().map(|(n, a)| json!([{"id": call_id, "type": "function", "function": {"name": n, "arguments": a}}]));
            let mut usage = json!({"prompt_tokens": tin, "completion_tokens": tout, "total_tokens": tin + tout, "prompt_tokens_details": {"cached_tokens": cached}});
            if provider == "deepseek" {
                usage["prompt_cache_hit_tokens"] = json!(cached);
                usage["prompt_cache_miss_tokens"] = json!(tin - cached);
            }
            if p.stream {
                let base = |delta: Value, fr: Value| data(&json!({"id": id, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [{"index": 0, "delta": delta, "finish_reason": fr}]}));
                let mut parts = vec![base(json!({"role": "assistant", "content": ""}), Value::Null)];
                for c in chunks(&out.text) {
                    parts.push(base(json!({"content": c}), Value::Null));
                }
                if let Some((n, a)) = &out.tool {
                    parts.push(base(json!({"tool_calls": [{"index": 0, "id": call_id, "type": "function", "function": {"name": n, "arguments": ""}}]}), Value::Null));
                    for c in a.as_bytes().chunks(12) {
                        parts.push(base(json!({"tool_calls": [{"index": 0, "function": {"arguments": String::from_utf8_lossy(c)}}]}), Value::Null));
                    }
                }
                parts.push(base(json!({}), json!(finish_reason)));
                if b.pointer("/stream_options/include_usage").and_then(|v| v.as_bool()).unwrap_or(false) {
                    parts.push(data(&json!({"id": id, "object": "chat.completion.chunk", "created": created, "model": model, "choices": [], "usage": usage})));
                }
                parts.push("data: [DONE]\n\n".into());
                raw = parts.concat();
                response = sse(parts, out.slow, out.cut, false);
            } else {
                let mut msg = json!({"role": "assistant", "content": if out.text.is_empty() && out.tool.is_some() { Value::Null } else { json!(out.text) }, "refusal": null});
                if let Some(t) = tool_calls {
                    msg["tool_calls"] = t;
                }
                let mut v = json!({"id": id, "object": "chat.completion", "created": created, "model": model, "choices": [{"index": 0, "message": msg, "finish_reason": finish_reason, "logprobs": null}], "usage": usage, "system_fingerprint": "fp_stuntman"});
                if provider == "azure-openai" {
                    v["prompt_filter_results"] = json!([{"prompt_index": 0, "content_filter_results": {}}]);
                }
                raw = v.to_string();
                response = http(200, v, vec![("x-request-id", rid("req_"))]);
            }
        }
    }
    finish(200, tout, cached, answer_text, raw, p.stream, out.why.clone()).await;
    response
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn prompts_rules_and_vectors() {
        let b = json!({"model": "gpt-6.1", "messages": [{"role": "system", "content": "You are the support agent of Acme."}, {"role": "user", "content": "I want a REFUND for A-1042"}],
            "tools": [{"type": "function", "function": {"name": "lookup_order"}}]});
        let p = prompt("chat", &b, "");
        assert_eq!((p.last_user.as_str(), p.tools.len(), p.tool_result), ("I want a REFUND for A-1042", 1, false));
        let cfg = config::Config { rules: vec![config::Rule { name: "refund".into(), when: config::When { contains: "refund".into(), ..Default::default() },
            answer: config::Reply { text: "Refund on its way.".into(), tool_call: Some(config::ToolCall { name: "lookup_order".into(), arguments: json!({"order_id": "A-1042"}) }), ..Default::default() } }], ..Default::default() };
        let o = decide(&cfg, "openai", "chat", &p);
        assert_eq!(o.tool.as_ref().unwrap().0, "lookup_order");
        let mut b2 = b.clone();
        b2["messages"].as_array_mut().unwrap().push(json!({"role": "tool", "tool_call_id": "c1", "content": "{\"status\": \"paid\"}"}));
        let o = decide(&cfg, "openai", "chat", &prompt("chat", &b2, ""));
        assert_eq!((o.tool.is_none(), o.text.as_str()), (true, "Refund on its way."));
        let (a, c) = (embedding("hello", 8), embedding("hello", 8));
        assert_eq!(a, c);
        assert!((a.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
        assert!(config::glob("gpt-*-mini", "gpt-6.1-mini") && !config::glob("gpt-*-mini", "gpt-6.1"));
    }
}
