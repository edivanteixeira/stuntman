//! A10b: a plugin as an MCP server. JSON-RPC 2.0 over Streamable HTTP (protocol 2025-06-18):
//! initialize, ping, tools/list, tools/call and notifications. Each tool is a route with `tool:`,
//! run with the usual steps; its arguments are the body.

use serde_json::{json, Value};
use stuntman_core::{util, Ctx};

use crate::{
    engine::{self, Outcome, Req},
    plugin::{Mcp, Plugin, Route},
    store,
};

pub const PROTOCOL: &str = "2025-06-18";

fn reply(m: &Mcp, status: u16, msg: Value, session: Option<String>) -> Outcome {
    let mut headers = vec![];
    if let Some(s) = session {
        headers.push(("mcp-session-id".to_string(), s));
    }
    let body = if m.sse {
        headers.push(("content-type".into(), "text/event-stream".into()));
        format!("event: message\ndata: {msg}\n\n").into_bytes()
    } else {
        headers.push(("content-type".into(), "application/json".into()));
        msg.to_string().into_bytes()
    };
    Outcome { status, headers, body, ..Default::default() }
}

fn rpc_err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The tools the server lists.
pub fn tools(p: &Plugin) -> Vec<Value> {
    p.routes
        .iter()
        .filter(|r| !r.tool.is_empty())
        .map(|r| json!({"name": r.tool, "description": r.description, "inputSchema": r.input_schema.clone().unwrap_or(json!({"type": "object"}))}))
        .collect()
}

pub async fn serve<'a>(ctx: &Ctx, p: &'a Plugin, m: &Mcp, method: &str, rq: &Req) -> (Option<&'a Route>, Outcome) {
    let svc = &p.service.name;
    let named = |o: Outcome, answer: &str, sentence: String| Outcome { answer: answer.into(), sentence, ..o };
    match method {
        "GET" => return (None, named(Outcome { status: 405, headers: vec![("allow".into(), "POST, DELETE".into())], ..Default::default() }, "405", "MCP: GET (no server stream offered)".into())),
        "DELETE" => return (None, named(Outcome { status: 200, ..Default::default() }, "session_ended", "MCP session ended by the app".into())),
        "POST" => {}
        _ => return (None, named(Outcome { status: 405, ..Default::default() }, "405", format!("MCP: {method} not allowed"))),
    }
    // Auth: a bearer token from this service's token route.
    if m.auth == "bearer" {
        let tok = rq.headers.get("authorization").and_then(|h| h.strip_prefix("Bearer ").or(h.strip_prefix("bearer "))).unwrap_or("").trim().to_string();
        let ok = match store::token(ctx, svc, &tok).await.ok().flatten() { Some((_, _, exp)) => exp >= util::now_ms(), None => false };
        if !ok {
            let meta = format!("{}/.well-known/oauth-protected-resource", p.service.base_url.trim_end_matches('/'));
            let o = Outcome { status: 401, headers: vec![("www-authenticate".into(), format!("Bearer resource_metadata=\"{meta}\"")), ("content-type".into(), "application/json".into())], body: br#"{"error":"invalid_token"}"#.to_vec(), ..Default::default() };
            return (None, named(o, "401", "MCP: no valid bearer token (401)".into()));
        }
    }
    let msg = &rq.body;
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let call = msg.get("method").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let session = rq.headers.get("mcp-session-id").cloned();
    // A forced session_expired: the session is gone (404), the app must initialize again.
    if call != "initialize" {
        if let Ok(Some(f)) = store::forced(ctx, svc).await {
            if f["answer"] == "session_expired" {
                let _ = store::take_force(ctx, svc, "", false).await;
                let o = Outcome { status: 404, provoked: Some(format!("forced by {}", f["by"].as_str().unwrap_or(""))), ..Default::default() };
                return (None, named(o, "session_expired", format!("MCP {call}: session expired (404)")));
            }
        }
    }
    if msg.get("jsonrpc").and_then(|v| v.as_str()) != Some("2.0") || call.is_empty() {
        return (None, named(reply(m, 400, rpc_err(&id, -32600, "Invalid Request"), None), "invalid_request", "MCP: not a JSON-RPC 2.0 request".into()));
    }
    if id.is_null() {
        // A notification: accepted, nothing to answer.
        return (None, named(Outcome { status: 202, ..Default::default() }, "accepted", format!("MCP notification {call}")));
    }
    match call.as_str() {
        "initialize" => {
            let v = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL).to_string();
            let who = params.pointer("/clientInfo/name").and_then(|v| v.as_str()).unwrap_or("a client").to_string();
            let mut result = json!({"protocolVersion": v, "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": if m.name.is_empty() { svc.clone() } else { m.name.clone() }, "version": m.version}});
            if !m.instructions.is_empty() {
                result["instructions"] = json!(m.instructions);
            }
            let sid = format!("mcp-{}", util::uuid());
            (None, named(reply(m, 200, json!({"jsonrpc": "2.0", "id": id, "result": result}), Some(sid)), "initialize", format!("MCP initialize by {who} (protocol {v})")))
        }
        "ping" => (None, named(reply(m, 200, json!({"jsonrpc": "2.0", "id": id, "result": {}}), session), "ping", "MCP ping".into())),
        "tools/list" => {
            let t = tools(p);
            let n = t.len();
            (None, named(reply(m, 200, json!({"jsonrpc": "2.0", "id": id, "result": {"tools": t}}), session), "tools/list", format!("MCP tools/list: {n} tools")))
        }
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let Some(r) = p.routes.iter().find(|r| r.tool == name) else {
                return (None, named(reply(m, 200, rpc_err(&id, -32602, &format!("Unknown tool: {name}")), session), "unknown_tool", format!("MCP tools/call {name}: no such tool")));
            };
            if let Some(schema) = &r.input_schema {
                if let Err(e) = crate::openapi::check(schema, &args, "", false) {
                    let res = json!({"content": [{"type": "text", "text": format!("Invalid arguments: {e}")}], "isError": true});
                    return (Some(r), named(reply(m, 200, json!({"jsonrpc": "2.0", "id": id, "result": res}), session), "invalid_arguments", format!("MCP {name}: arguments break the schema ({e})")));
                }
            }
            let mut req = rq.clone();
            req.body = args;
            let o = engine::run(ctx, p, r, Default::default(), &req, false).await;
            if o.network.is_some() {
                return (Some(r), o);
            }
            let text = String::from_utf8_lossy(&o.body).to_string();
            let a = p.answers.iter().find(|a| a.name == o.answer);
            let msg = match a.and_then(|a| a.rpc_error) {
                Some(code) => rpc_err(&id, code, &text),
                None => {
                    let structured = serde_json::from_str::<Value>(&text).ok().filter(|v| v.is_object());
                    let mut res = json!({"content": [{"type": "text", "text": text}], "isError": o.status >= 400});
                    if let Some(s) = structured {
                        res["structuredContent"] = s;
                    }
                    json!({"jsonrpc": "2.0", "id": id, "result": res})
                }
            };
            let mut out = reply(m, 200, msg, session);
            out.answer = o.answer.clone();
            out.sentence = format!("MCP {name}: {}", o.sentence);
            out.steps = o.steps;
            out.scope = o.scope;
            out.provoked = o.provoked;
            out.sent = o.sent;
            out.latency_ms = o.latency_ms;
            (Some(r), out)
        }
        other => (None, named(reply(m, 200, rpc_err(&id, -32601, &format!("Method not found: {other}")), session), "method_not_found", format!("MCP {other}: method not found"))),
    }
}
