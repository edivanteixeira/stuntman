//! A8 · Services from an OpenAPI 3.x or Swagger 2.0 spec: each operation becomes a route with its
//! success answer (the spec's example, or one built from the schema), the other responses become
//! named answers, request bodies are checked against their schema, and OAuth client credentials
//! get a token route. The spec is kept next to the plugin and served back.
//! ponytail: JSON bodies only (form bodies are checked leniently); path and query parameters are
//! not checked; no discriminator, pattern or length checks.

use std::collections::{BTreeMap, HashMap};

use serde_json::{json, Map, Value};
use stuntman_core::{ApiError, Ctx, Tool};

use crate::{
    plugin::{self, Answer, Decide, OAuth, Plugin, Receive, Route, Spec},
    soap::{base_name, fetch, join, snake},
};

pub fn folder(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    plugin::dir(ctx).join(format!("{service}.spec"))
}

/// YAML (and JSON, its subset) into JSON; integer keys like `200:` become strings.
fn y2j(v: serde_yaml::Value) -> Value {
    use serde_yaml::Value as Y;
    match v {
        Y::Null => Value::Null,
        Y::Bool(b) => json!(b),
        Y::Number(n) => n.as_i64().map(|x| json!(x)).or(n.as_u64().map(|x| json!(x))).unwrap_or_else(|| json!(n.as_f64())),
        Y::String(s) => json!(s),
        Y::Sequence(a) => Value::Array(a.into_iter().map(y2j).collect()),
        Y::Mapping(m) => Value::Object(m.into_iter().map(|(k, v)| (match k { Y::String(s) => s, Y::Number(n) => n.to_string(), Y::Bool(b) => b.to_string(), o => serde_yaml::to_string(&o).unwrap_or_default().trim().to_string() }, y2j(v))).collect()),
        Y::Tagged(t) => y2j(t.value),
    }
}

pub fn parse(text: &str) -> Result<Value, String> {
    serde_yaml::from_str::<serde_yaml::Value>(text).map(y2j).map_err(|e| e.to_string())
}

/// Is this text an OpenAPI or Swagger document (and not a WSDL)?
pub fn is_spec(text: &str) -> bool {
    let t = text.trim_start();
    !t.starts_with('<') && parse(t).map(|v| v.get("openapi").is_some() || v.get("swagger").is_some()).unwrap_or(false)
}

/// Every document the spec points to ($ref "other.yaml#/…"), by its full location.
struct Docs {
    docs: HashMap<String, Value>,
}

impl Docs {
    fn pointer<'a>(&'a self, doc: &str, ptr: &str) -> Option<&'a Value> {
        let d = self.docs.get(doc)?;
        if ptr.is_empty() || ptr == "/" {
            return Some(d);
        }
        d.pointer(&ptr.replace("~1", "\u{0}").replace('\u{0}', "~1"))
    }

    /// The value with every $ref inlined; a cycle (or depth past 12) becomes {}.
    fn resolve(&self, v: &Value, doc: &str, stack: &mut Vec<String>) -> Value {
        if stack.len() > 12 {
            return json!({});
        }
        match v {
            Value::Object(o) => {
                if let Some(r) = o.get("$ref").and_then(|r| r.as_str()) {
                    let (file, ptr) = r.split_once('#').unwrap_or((r, ""));
                    let target = if file.is_empty() { doc.to_string() } else { join(doc, file) };
                    let key = format!("{target}#{ptr}");
                    if stack.contains(&key) {
                        return json!({});
                    }
                    let Some(t) = self.pointer(&target, ptr).cloned() else { return json!({}) };
                    stack.push(key);
                    let out = self.resolve(&t, &target, stack);
                    stack.pop();
                    return out;
                }
                Value::Object(o.iter().map(|(k, x)| (k.clone(), self.resolve(x, doc, stack))).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(|x| self.resolve(x, doc, stack)).collect()),
            x => x.clone(),
        }
    }
}

fn refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(o) => {
            if let Some(r) = o.get("$ref").and_then(|r| r.as_str()) {
                let f = r.split('#').next().unwrap_or("");
                if !f.is_empty() {
                    out.push(f.to_string());
                }
            }
            o.values().for_each(|x| refs(x, out));
        }
        Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
        _ => {}
    }
}

/// The name a $ref gives a schema: "#/components/schemas/ChargeRequest" → ChargeRequest.
fn ref_name(v: &Value) -> Option<String> {
    v.get("$ref").and_then(|r| r.as_str()).map(|r| r.rsplit('/').next().unwrap_or(r).trim_end_matches(".yaml").trim_end_matches(".json").to_string())
}

fn pascal(s: &str) -> String {
    snake(s).split('_').filter(|w| !w.is_empty()).map(|w| format!("{}{}", w[..1].to_uppercase(), &w[1..])).collect()
}

/// Answer names for HTTP statuses: 402 → payment_required.
pub fn status_name(code: u16) -> String {
    match code {
        200 => "ok", 201 => "created", 202 => "accepted", 204 => "no_content",
        400 => "bad_request", 401 => "unauthorized", 402 => "payment_required", 403 => "forbidden", 404 => "not_found", 405 => "not_allowed",
        409 => "conflict", 410 => "gone", 412 => "precondition_failed", 415 => "unsupported_media", 422 => "invalid", 429 => "rate_limited",
        500 => "server_error", 501 => "not_implemented", 502 => "bad_gateway", 503 => "unavailable", 504 => "gateway_timeout",
        c => return format!("status_{c}"),
    }
    .into()
}

/// A name no other answer has, unless that answer is the same (then it is shared).
pub fn uniq_answer(answers: &[Answer], tries: &[String], a: &Answer) -> (String, bool) {
    for t in tries {
        match answers.iter().find(|x| &x.name == t) {
            None => return (t.clone(), false),
            Some(x) if x.status == a.status && x.body == a.body && x.headers == a.headers && x.file == a.file => return (t.clone(), true),
            _ => {}
        }
    }
    let base = tries.last().cloned().unwrap_or("answer".into());
    ((2..).map(|i| format!("{base}_{i}")).find(|n| !answers.iter().any(|x| &x.name == n)).unwrap(), false)
}

/// A value that fits the schema: its example, enum, format or type.
pub fn sample(s: &Value, depth: usize) -> Value {
    if let Some(e) = s.get("example").or(s.get("default")).or(s.get("const")) {
        return e.clone();
    }
    if let Some(e) = s.get("examples").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    if let Some(e) = s.get("enum").and_then(|e| e.as_array()).and_then(|a| a.first()) {
        return e.clone();
    }
    for k in ["allOf"] {
        if let Some(a) = s.get(k).and_then(|a| a.as_array()) {
            let mut o = Map::new();
            for x in a {
                if let Value::Object(m) = sample(x, depth) {
                    o.extend(m);
                }
            }
            return Value::Object(o);
        }
    }
    for k in ["oneOf", "anyOf"] {
        if let Some(x) = s.get(k).and_then(|a| a.as_array()).and_then(|a| a.first()) {
            return sample(x, depth);
        }
    }
    let ty = match s.get("type") {
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str()).find(|t| *t != "null").unwrap_or("string").to_string(),
        Some(t) => t.as_str().unwrap_or("string").to_string(),
        None if s.get("properties").is_some() => "object".into(),
        None if s.get("items").is_some() => "array".into(),
        None => return json!({}),
    };
    match ty.as_str() {
        "object" => {
            if depth > 5 {
                return json!({});
            }
            let props = s.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
            Value::Object(props.iter().map(|(k, v)| (k.clone(), sample_field(k, v, depth + 1))).collect())
        }
        "array" => if depth > 5 { json!([]) } else { json!([sample(s.get("items").unwrap_or(&json!({})), depth + 1)]) },
        "integer" => s.get("minimum").cloned().unwrap_or(json!(1)),
        "number" => s.get("minimum").cloned().unwrap_or(json!(1.5)),
        "boolean" => json!(true),
        _ => match s.get("format").and_then(|f| f.as_str()).unwrap_or("") {
            "date-time" => json!("{{ now() }}"),
            "date" => json!("2026-01-01"),
            "email" => json!("user@example.com"),
            "uuid" => json!("{{ uuid() }}"),
            "uri" | "url" => json!("https://example.com/"),
            _ => json!("string"),
        },
    }
}

fn sample_field(name: &str, s: &Value, depth: usize) -> Value {
    let v = sample(s, depth);
    if v == json!("string") {
        let n = name.to_lowercase();
        if n == "id" || n.ends_with("_id") {
            return json!(format!("{}_{{{{ random_id() }}}}", &n.trim_end_matches("_id").chars().take(3).collect::<String>()));
        }
        if n.contains("email") {
            return json!("user@example.com");
        }
        return json!(name);
    }
    v
}

/// Request fields echoed in an answer: "amount" in both → {{ body.amount | json }}.
const ECHO: &str = "@@echo:";

fn mark_echo(v: &mut Value, req: &Map<String, Value>) {
    if let Value::Object(o) = v {
        for (k, x) in o.iter_mut() {
            if k == "id" {
                continue;
            }
            if let Some(r) = req.get(k) {
                if !type_of(r).is_empty() && type_of(r) == json_type(x) && !x.is_object() && !x.is_array() {
                    *x = echo(&format!("body.{k}"));
                }
            }
        }
    }
}

fn type_of(schema: &Value) -> &'static str {
    match schema.get("type").and_then(|t| t.as_str()).unwrap_or("") {
        "integer" | "number" => "number",
        "boolean" => "boolean",
        "string" => "string",
        "array" => "array",
        "object" => "object",
        _ => "",
    }
}

fn json_type(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        _ => "",
    }
}

/// A value echoed from the request: `{{ body.amount | json }}` once templated.
pub(crate) fn echo(expr: &str) -> Value {
    json!(format!("{ECHO}{expr}@@"))
}

/// Pretty JSON with the echo marks turned into template holes.
pub fn template(v: &Value) -> String {
    let t = serde_json::to_string_pretty(v).unwrap_or_default();
    let mut out = String::new();
    let mut rest = t.as_str();
    while let Some(i) = rest.find(&format!("\"{ECHO}")) {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1 + ECHO.len()..];
        let Some(j) = after.find("@@\"") else { break };
        out.push_str(&format!("{{{{ {} | json }}}}", &after[..j]));
        rest = &after[j + 3..];
    }
    out.push_str(rest);
    out
}

// ---------------------------------------------------------------- the body check

/// The body against a JSON Schema: types, required, enum, additionalProperties: false, items,
/// allOf / anyOf / oneOf, and the date, date-time and email formats. `lenient`: form bodies,
/// where every value is text.
pub fn check(s: &Value, v: &Value, at: &str, lenient: bool) -> Result<(), String> {
    let name = |k: &str| if at.is_empty() { k.to_string() } else { format!("{at}.{k}") };
    let here = || if at.is_empty() { "the body".to_string() } else { at.to_string() };
    if let Some(a) = s.get("allOf").and_then(|a| a.as_array()) {
        for x in a {
            check(x, v, at, lenient)?;
        }
    }
    for k in ["anyOf", "oneOf"] {
        if let Some(a) = s.get(k).and_then(|a| a.as_array()) {
            let errs: Vec<String> = a.iter().filter_map(|x| check(x, v, at, lenient).err()).collect();
            if errs.len() == a.len() && !a.is_empty() {
                return Err(errs[0].clone());
            }
        }
    }
    if v.is_null() {
        let nullable = s.get("nullable") == Some(&json!(true)) || s.get("type").and_then(|t| t.as_array()).map(|a| a.contains(&json!("null"))).unwrap_or(false);
        return if nullable || s.get("type").is_none() { Ok(()) } else { Err(format!("{} is null", here())) };
    }
    if let Some(e) = s.get("enum").and_then(|e| e.as_array()) {
        if !e.contains(v) && !(lenient && e.iter().any(|x| x.as_str().map(|x| Some(x) == v.as_str()).unwrap_or(x.to_string() == v.as_str().unwrap_or("")))) {
            return Err(format!("{}: {} is not one of {}", here(), v, e.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")));
        }
    }
    let types: Vec<String> = match s.get("type") {
        Some(Value::Array(a)) => a.iter().filter_map(|t| t.as_str().map(String::from)).collect(),
        Some(Value::String(t)) => vec![t.clone()],
        _ if s.get("properties").is_some() => vec!["object".into()],
        _ => vec![],
    };
    let text = v.as_str().unwrap_or("");
    let fits = |t: &str| match t {
        "string" => v.is_string(),
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false) || (lenient && text.parse::<i64>().is_ok()),
        "number" => v.is_number() || (lenient && text.parse::<f64>().is_ok()),
        "boolean" => v.is_boolean() || (lenient && (text == "true" || text == "false")),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    };
    if !types.is_empty() && !types.iter().any(|t| fits(t)) {
        return Err(format!("{}: expected {}, got {}", here(), types.join(" or "), serde_json::to_string(v).unwrap_or_default().chars().take(40).collect::<String>()));
    }
    if let (Some(f), Some(t)) = (s.get("format").and_then(|f| f.as_str()), v.as_str()) {
        let date = |d: &str| d.len() >= 10 && d.as_bytes()[4] == b'-' && d.as_bytes()[7] == b'-' && d[..4].parse::<u32>().is_ok() && d[5..7].parse::<u32>().map(|m| (1..=12).contains(&m)).unwrap_or(false) && d[8..10].parse::<u32>().map(|x| (1..=31).contains(&x)).unwrap_or(false);
        let ok = match f {
            "date" => t.len() == 10 && date(t),
            "date-time" => t.len() > 10 && date(t) && matches!(t.as_bytes()[10], b'T' | b't' | b' '),
            "email" => t.split_once('@').map(|(a, b)| !a.is_empty() && b.contains('.')).unwrap_or(false),
            _ => true,
        };
        if !ok {
            return Err(format!("{}: “{t}” is not a {f}", here()));
        }
    }
    if let Value::Object(o) = v {
        for r in s.get("required").and_then(|r| r.as_array()).into_iter().flatten().filter_map(|r| r.as_str()) {
            if !o.contains_key(r) {
                return Err(format!("{} is required", name(r)));
            }
        }
        let props = s.get("properties").and_then(|p| p.as_object());
        for (k, x) in o {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => check(ps, x, &name(k), lenient)?,
                None if s.get("additionalProperties") == Some(&json!(false)) => return Err(format!("{} is not a field of {}", name(k), here())),
                None => {
                    if let Some(ap) = s.get("additionalProperties").filter(|a| a.is_object()) {
                        check(ap, x, &name(k), lenient)?;
                    }
                }
            }
        }
    }
    if let (Value::Array(a), Some(it)) = (v, s.get("items")) {
        for (i, x) in a.iter().enumerate() {
            check(it, x, &format!("{}[{i}]", if at.is_empty() { "body" } else { at }), lenient)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- the import

pub struct Imported {
    pub plugin: Plugin,
    pub files: Vec<(String, String)>,
    pub warnings: Vec<String>,
    pub webhooks: Vec<String>,
}

const METHODS: [&str; 6] = ["get", "put", "post", "delete", "patch", "head"];

fn json_content(c: &Value) -> Option<(String, Value)> {
    let o = c.as_object()?;
    o.iter().find(|(k, _)| k.contains("json")).or(o.iter().next()).map(|(k, v)| (k.clone(), v.clone()))
}

/// Reads a spec (URL, file path, or its text) and the files its $refs point to into a plugin.
pub async fn import(http: &reqwest::Client, service: &str, source: &str, text: Option<String>) -> Result<Imported, ApiError> {
    plugin::valid_name(service)?;
    let text = match text {
        Some(t) => t,
        None => fetch(http, source).await.map_err(|e| ApiError::bad(format!("Could not read the spec: {e}")))?,
    };
    let root = parse(&text).map_err(|e| ApiError::bad(format!("Not YAML or JSON: {e}")))?;
    let version = root.get("openapi").or(root.get("swagger")).map(|v| v.as_str().map(String::from).unwrap_or(v.to_string())).ok_or_else(|| ApiError::bad("Not an OpenAPI or Swagger spec (no openapi: or swagger: at the top)"))?;
    let v2 = version.starts_with('2');
    let src = if source.is_empty() { "openapi.yaml".to_string() } else { source.to_string() };
    let fname = { let b = base_name(&src); if b.contains('.') { b } else if text.trim_start().starts_with('{') { "openapi.json".into() } else { "openapi.yaml".into() } };
    // Every file the $refs reach, fetched relative to the one that points to it.
    let mut docs = Docs { docs: HashMap::new() };
    let mut files = vec![(fname.clone(), text.clone())];
    let mut warnings = vec![];
    let mut queue = vec![(src.clone(), root.clone())];
    while let Some((loc, v)) = queue.pop() {
        let mut out = vec![];
        refs(&v, &mut out);
        docs.docs.insert(loc.clone(), v);
        for f in out {
            let full = join(&loc, &f);
            if docs.docs.contains_key(&full) || queue.iter().any(|q| q.0 == full) {
                continue;
            }
            match fetch(http, &full).await.map_err(|e| e).and_then(|t| parse(&t).map(|v| (t, v))) {
                Ok((t, v)) => {
                    let rel = f.trim_start_matches("./");
                    files.push((if f.contains("://") || rel.contains("..") || rel.starts_with('/') { base_name(&f) } else { rel.to_string() }, t));
                    queue.push((full, v));
                }
                Err(e) => {
                    warnings.push(format!("{} could not be read ({e}): what points to it answers {{}}", base_name(&f)));
                    docs.docs.insert(full, json!({}));
                }
            }
        }
    }
    let res = |v: &Value| docs.resolve(v, &src, &mut vec![]);

    // The server: base URL, host and the path every route starts with.
    let server = if v2 {
        let scheme = root.get("schemes").and_then(|s| s.as_array()).and_then(|a| a.first()).and_then(|s| s.as_str()).unwrap_or("https");
        root.get("host").and_then(|h| h.as_str()).map(|h| format!("{scheme}://{h}{}", root.get("basePath").and_then(|b| b.as_str()).unwrap_or(""))).unwrap_or_default()
    } else {
        let s0 = root.pointer("/servers/0").cloned().unwrap_or(Value::Null);
        let mut u = s0.get("url").and_then(|u| u.as_str()).unwrap_or("").to_string();
        for (k, var) in s0.get("variables").and_then(|v| v.as_object()).cloned().unwrap_or_default() {
            u = u.replace(&format!("{{{k}}}"), var.get("default").and_then(|d| d.as_str()).unwrap_or(""));
        }
        if u.starts_with('/') && source.contains("://") {
            let origin: String = source.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
            u = format!("{origin}{u}");
        }
        u
    };
    let (host, base_path) = match server.split_once("://") {
        Some((_, rest)) => {
            let (h, p) = rest.split_once('/').unwrap_or((rest, ""));
            (h.split(':').next().unwrap_or("").to_string(), format!("/{p}").trim_end_matches('/').to_string())
        }
        None => (String::new(), server.trim_end_matches('/').to_string()),
    };

    // Security: OAuth2 client credentials gets a token route; Basic checks users (any by default).
    let schemes = res(root.pointer(if v2 { "/securityDefinitions" } else { "/components/securitySchemes" }).unwrap_or(&json!({})));
    let mut token_path = None;
    let mut basic = false;
    let mut oauth_schemes = vec![];
    for (n, sc) in schemes.as_object().cloned().unwrap_or_default() {
        match sc.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "oauth2" => {
                let url = if v2 { (sc.get("flow").and_then(|f| f.as_str()) == Some("application")).then(|| sc.get("tokenUrl").and_then(|t| t.as_str()).unwrap_or("").to_string()) } else { sc.pointer("/flows/clientCredentials/tokenUrl").and_then(|t| t.as_str()).map(String::from) };
                if let Some(u) = url.filter(|u| !u.is_empty()) {
                    let p = match u.split_once("://") { Some((_, r)) => format!("/{}", r.split_once('/').map(|x| x.1).unwrap_or("")), None => u.clone() };
                    token_path = Some(p);
                    oauth_schemes.push(n);
                }
            }
            "basic" => basic = true,
            "http" if sc.get("scheme").and_then(|s| s.as_str()).map(|s| s.eq_ignore_ascii_case("basic")).unwrap_or(false) => basic = true,
            _ => {}
        }
    }
    let uses_oauth = |sec: &Value| sec.as_array().map(|a| a.iter().any(|req| req.as_object().map(|o| o.keys().any(|k| oauth_schemes.contains(k))).unwrap_or(false))).unwrap_or(false);
    let root_sec = root.get("security").cloned().unwrap_or(json!([]));

    let mut routes: Vec<Route> = vec![];
    let mut answers: Vec<Answer> = vec![];
    let mut schemas: BTreeMap<String, Value> = BTreeMap::new();
    let paths = root.get("paths").and_then(|p| p.as_object()).cloned().unwrap_or_default();
    for (path, raw_item) in &paths {
        let item = res(raw_item);
        for m in METHODS {
            let Some(op) = item.get(m) else { continue };
            let route = format!("{} {base_path}{path}", m.to_uppercase());
            let opid = op.get("operationId").and_then(|o| o.as_str()).map(String::from).unwrap_or_else(|| snake(&format!("{m} {}", path.split('/').filter(|s| !s.starts_with('{')).collect::<Vec<_>>().join(" "))));
            let mut r = Route { route: route.clone(), operation: opid.clone(), ..Default::default() };
            let sec = op.get("security").cloned().unwrap_or(root_sec.clone());
            if uses_oauth(&sec) {
                r.receive.auth = "bearer".into();
            } else if basic && sec.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
                r.receive.auth = "basic".into();
            }
            // The request body's schema, named by its $ref or after the operation.
            let raw_schema = if v2 {
                op.get("parameters").and_then(|p| p.as_array()).and_then(|a| a.iter().map(|x| docs.resolve(x, &src, &mut vec![])).find(|x| x.get("in") == Some(&json!("body")))).and_then(|b| b.get("schema").cloned())
            } else {
                let rb = res(op.get("requestBody").unwrap_or(&Value::Null));
                // keep the $ref (for the name): read it unresolved first
                raw_item.get(m).and_then(|o| o.pointer("/requestBody/content")).and_then(json_content).and_then(|(_, c)| c.get("schema").cloned()).or_else(|| rb.get("content").and_then(json_content).and_then(|(_, c)| c.get("schema").cloned()))
            };
            let mut req_props = Map::new();
            if let Some(rs) = raw_schema {
                let name = ref_name(&rs).unwrap_or_else(|| format!("{}Request", pascal(&opid)));
                let full = res(&rs);
                req_props = full.get("properties").and_then(|p| p.as_object()).cloned().unwrap_or_default();
                schemas.insert(name.clone(), full);
                r.receive.schema = name;
            }
            // Responses: the first success is the default answer; the others are named answers.
            let mut first_ok: Option<String> = None;
            let mut first_any: Option<String> = None;
            let resps = op.get("responses").and_then(|x| x.as_object()).cloned().unwrap_or_default();
            for (code, resp) in &resps {
                let Ok(status) = code.parse::<u16>() else { continue };
                let resp = res(resp);
                let (ctype, from, mut body) = if v2 {
                    match resp.pointer("/examples/application~1json").cloned() {
                        Some(e) => ("application/json".to_string(), "example", e),
                        None => match resp.get("schema") { Some(s) => ("application/json".into(), "schema", sample(s, 0)), None => (String::new(), "", Value::Null) },
                    }
                } else {
                    match resp.get("content").and_then(json_content) {
                        Some((ct, c)) => match c.get("example").cloned().or_else(|| c.get("examples").and_then(|e| e.as_object()).and_then(|o| o.values().next()).and_then(|e| e.get("value").cloned())) {
                            Some(e) => (ct, "example", e),
                            None => (ct, "schema", c.get("schema").map(|s| sample(s, 0)).unwrap_or(Value::Null)),
                        },
                        None => (String::new(), "", Value::Null),
                    }
                };
                let json = ctype.contains("json");
                if !json && from == "schema" {
                    body = Value::Null;
                }
                if from == "schema" && (200..300).contains(&status) {
                    mark_echo(&mut body, &req_props);
                }
                let mut headers = BTreeMap::new();
                if !ctype.is_empty() {
                    headers.insert("content-type".to_string(), ctype.clone());
                }
                for (h, hv) in resp.get("headers").and_then(|h| h.as_object()).cloned().unwrap_or_default() {
                    if let Some(e) = hv.get("example").or(hv.pointer("/schema/example")).and_then(|e| e.as_str()) {
                        headers.insert(h.to_lowercase(), e.to_string());
                    }
                }
                let text = if body.is_null() { String::new() } else if json { template(&body) } else { body.as_str().map(String::from).unwrap_or(body.to_string()) };
                let a = Answer { name: String::new(), status, headers, body: text,
                    note: format!("responses.{code} of {opid}{}", match from { "example" => " · the spec's example", "schema" => " · built from the schema", _ => if ctype.is_empty() { "" } else { " · not JSON: add the file" } }), ..Default::default() };
                let sn = status_name(status);
                let tries = if (200..300).contains(&status) { vec![snake(&opid), format!("{}_{sn}", snake(&opid))] } else { vec![sn.clone(), format!("{}_{sn}", snake(&opid))] };
                let (name, shared) = uniq_answer(&answers, &tries, &a);
                if !shared {
                    answers.push(Answer { name: name.clone(), ..a });
                }
                if (200..300).contains(&status) && first_ok.is_none() {
                    first_ok = Some(name.clone());
                }
                if first_any.is_none() {
                    first_any = Some(name.clone());
                }
                if r.receive.invalid.is_empty() && (status == 400 || status == 422) && !r.receive.schema.is_empty() {
                    r.receive.invalid = name;
                }
            }
            let def = match first_ok.or(first_any) {
                Some(d) => d,
                None => {
                    let a = Answer { name: String::new(), status: 200, body: "{}".into(), note: format!("{opid}: the spec declares no response"), ..Default::default() };
                    let (n, shared) = uniq_answer(&answers, &[snake(&opid)], &a);
                    if !shared {
                        answers.push(Answer { name: n.clone(), ..a });
                    }
                    n
                }
            };
            r.decide = vec![Decide { when: None, answer: def, ..Default::default() }];
            routes.push(r);
        }
    }
    // The token route of OAuth client credentials (the spec's own path when it has one).
    if let Some(tp) = &token_path {
        match routes.iter_mut().find(|r| r.route.eq_ignore_ascii_case(&format!("POST {tp}"))) {
            Some(r) => {
                r.token = true;
                r.receive = Receive::default();
            }
            None => routes.push(Route { route: format!("POST {tp}"), token: true, ..Default::default() }),
        }
    }
    if routes.is_empty() {
        return Err(ApiError::bad("The spec has no paths"));
    }
    // Webhooks (3.1) and callbacks (3.0): their payloads, ready to send to the app.
    let mut webhooks = vec![];
    let mut hooks: Vec<(String, Value)> = root.get("webhooks").and_then(|w| w.as_object()).cloned().unwrap_or_default().into_iter().collect();
    for item in paths.values() {
        for m in METHODS {
            for (n, cb) in item.get(m).and_then(|o| o.get("callbacks")).and_then(|c| c.as_object()).cloned().unwrap_or_default() {
                if let Some(first) = res(&cb).as_object().and_then(|o| o.values().next().cloned()) {
                    hooks.push((n, first));
                }
            }
        }
    }
    for (n, item) in hooks {
        let item = res(&item);
        let Some(op) = METHODS.iter().find_map(|m| item.get(*m)) else { continue };
        let schema = op.pointer("/requestBody/content").and_then(json_content).and_then(|(_, c)| c.get("example").cloned().or(c.get("schema").map(|s| sample(s, 0))));
        let name = format!("webhook_{}", snake(&n));
        if !answers.iter().any(|a| a.name == name) {
            answers.push(Answer { name, status: 200, headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]), body: schema.map(|s| template(&s)).unwrap_or("{}".into()),
                note: format!("the {n} webhook the spec declares: its payload, for a route's send"), ..Default::default() });
        }
        webhooks.push(n);
    }
    let info = root.get("info").cloned().unwrap_or(json!({}));
    let mut service = plugin::Service { name: service.into(), title: info.get("title").and_then(|t| t.as_str()).unwrap_or(service).to_string(), origin: "yours".into(),
        description: info.get("description").and_then(|d| d.as_str()).unwrap_or("").lines().next().unwrap_or("").chars().take(120).collect(),
        base_url: if host.is_empty() { String::new() } else { format!("https://{host}") }, real_host: host.into(), ..Default::default() };
    if token_path.is_some() {
        service.auth.oauth2_client_credentials = Some(OAuth { any_client: true, token_ttl: "60m".into(), ..Default::default() });
    }
    if basic {
        service.auth.basic = Some(plugin::Basic { any_user: true, ..Default::default() });
    }
    let p = Plugin {
        service,
        routes,
        answers,
        spec: Some(Spec { openapi: fname, source: if source.contains("://") { source.into() } else { String::new() }, version, api_version: info.get("version").map(|v| v.as_str().map(String::from).unwrap_or(v.to_string())).unwrap_or_default(), validate: true, schemas }),
        ..Default::default()
    };
    Ok(Imported { plugin: p, files, warnings, webhooks })
}

/// Saves an import (and its files). `merge`: re-import, keeping steps, answers and settings.
pub fn save(ctx: &Ctx, mut im: Imported, merge: Option<Plugin>) -> Result<Value, ApiError> {
    let mut added = vec![];
    let mut removed = vec![];
    if let Some(old) = merge {
        let fresh = std::mem::take(&mut im.plugin.routes);
        for mut r in fresh {
            match old.routes.iter().find(|o| o.route.eq_ignore_ascii_case(&r.route)) {
                Some(o) => {
                    let (schema, invalid, opid) = (r.receive.schema.clone(), r.receive.invalid.clone(), r.operation.clone());
                    r = o.clone();
                    r.operation = opid;
                    if !r.receive.schema.is_empty() || r.receive.invalid.is_empty() {
                        r.receive.schema = schema;
                    }
                    if r.receive.invalid.is_empty() {
                        r.receive.invalid = invalid;
                    }
                }
                None => added.push(r.route.clone()),
            }
            im.plugin.routes.push(r);
        }
        // Routes the new spec does not have stay (hand-made ones too); they are listed.
        for o in &old.routes {
            if !im.plugin.routes.iter().any(|n| n.route.eq_ignore_ascii_case(&o.route)) {
                removed.push(o.route.clone());
                im.plugin.routes.push(o.clone());
            }
        }
        let fresh = std::mem::take(&mut im.plugin.answers);
        im.plugin.answers = old.answers.clone();
        for a in fresh {
            if !im.plugin.answers.iter().any(|x| x.name == a.name) {
                im.plugin.answers.push(a);
            }
        }
        if let (Some(n), Some(o)) = (im.plugin.spec.as_mut(), old.spec.as_ref()) {
            n.validate = o.validate;
            for (k, v) in &o.schemas {
                n.schemas.entry(k.clone()).or_insert_with(|| v.clone());
            }
        }
        im.plugin.service = old.service.clone();
        im.plugin.data = old.data.clone();
        im.plugin.pages = old.pages.clone();
    }
    let name = im.plugin.service.name.clone();
    let dir = folder(ctx, &name);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
    for (f, t) in &im.files {
        if let Some(parent) = dir.join(f).parent() {
            std::fs::create_dir_all(parent).map_err(|e| ApiError::internal(e.to_string()))?;
        }
        std::fs::write(dir.join(f), t).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    let v = plugin::save(ctx, &im.plugin)?;
    let p = &im.plugin;
    Ok(json!({"service": name, "kind": "openapi", "version": v, "spec": p.spec.as_ref().map(|s| json!({"version": s.version, "file": s.openapi, "apiVersion": s.api_version})),
        "routes": p.routes.iter().map(|r| json!({"route": r.route, "operation": r.operation, "schema": r.receive.schema, "answer": r.decide.first().map(|d| d.answer.clone()), "token": r.token})).collect::<Vec<_>>(),
        "answers": p.answers.iter().map(|a| json!({"name": a.name, "status": a.status})).collect::<Vec<_>>(),
        "webhooks": im.webhooks, "warnings": im.warnings, "added": added, "notInSpec": removed, "file": plugin::rel(&name)}))
}

/// GET /<spec file> on a service made from a spec: the spec, and the files it points to.
pub fn serve_file(ctx: &Ctx, p: &Plugin, inner: &str) -> Option<axum::response::Response> {
    use axum::response::IntoResponse;
    let sp = p.spec.as_ref()?;
    let name = inner.trim_start_matches('/');
    if name.is_empty() || name.contains("..") || name.contains('\\') {
        return None;
    }
    let t = std::fs::read_to_string(folder(ctx, &p.service.name).join(name)).ok()?;
    let ct = if name.ends_with(".json") || (name == sp.openapi && t.trim_start().starts_with('{')) { "application/json" } else { "application/yaml" };
    Some(([("content-type", ct)], t).into_response())
}

// ---------------------------------------------------------------- tools (A8 "API & MCP")

async fn read_source(ctx: &Ctx, source: &str, text: Option<String>) -> Result<String, ApiError> {
    match text {
        Some(t) => Ok(t),
        None => fetch(&ctx.http, source).await.map_err(|e| ApiError::bad(format!("Could not read the spec: {e}"))),
    }
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("import_spec", "A service from the provider's spec: OpenAPI 3.x or Swagger 2.0 (JSON or YAML), or a WSDL (then it is a SOAP service, as import_wsdl). Routes with their success answer (the spec's example or one built from the schema), the other responses as named answers, bodies checked against the schemas, OAuth client credentials with a token route. The spec is served back.", |ctx, a| async move {
            let s = a.need("service")?;
            if plugin::file(&ctx, &s).exists() {
                return Err(ApiError::new(409, format!("{s} is already installed; reimport_spec updates it")));
            }
            let src = a.str("source").unwrap_or_default();
            if src.is_empty() && a.str("spec").is_none() {
                return Err(ApiError::bad("source (a URL or a file) or spec (the text) is required"));
            }
            let text = read_source(&ctx, &src, a.str("spec")).await?;
            let mut v = if text.trim_start().starts_with('<') {
                let mut im = crate::soap::import(&ctx.http, &s, &src, Some(text)).await?;
                if let Some(t) = a.str("title") { im.plugin.service.title = t; }
                if let Some(h) = a.str("real_host") { im.plugin.service.real_host = h.into(); }
                let mut v = crate::soap::save(&ctx, im, None)?;
                v["kind"] = json!("wsdl");
                v
            } else {
                let mut im = import(&ctx.http, &s, &src, Some(text)).await?;
                if let Some(t) = a.str("title") { im.plugin.service.title = t; }
                if let Some(h) = a.str("real_host") { im.plugin.service.real_host = h.into(); }
                if a.bool("validate") == Some(false) { if let Some(sp) = im.plugin.spec.as_mut() { sp.validate = false; } }
                save(&ctx, im, None)?
            };
            v["note"] = json!("Every route gives its success answer until you add rules; force_answer gives the others.");
            Ok(v)
        })
        .req("service", "the new service's name, e.g. example-payments").param("source", "the spec's (or WSDL's) URL or file path").param("spec", "the text, instead of source").param("title", "shown name").param("real_host", "the provider's host (default: from the spec's servers)").param("validate", "false: do not check bodies against the schemas")
        .route("POST", "/plugins/spec"),
        Tool::new("reimport_spec", "Reads the spec again (or a new one): new routes start with their success answer; existing ones keep their steps; answers, data and service settings stay. Lists what was added and removed. Works for WSDLs too.", |ctx, a| async move {
            let s = a.need("service")?;
            let old = plugin::get(&ctx, &s)?;
            if old.soap.is_some() {
                let sp = old.soap.clone().unwrap();
                let src = a.str("source").unwrap_or(sp.source.clone());
                let text = match a.str("spec") { Some(t) => Some(t), None if src.is_empty() => std::fs::read_to_string(crate::soap::folder(&ctx, &s).join(&sp.wsdl)).ok(), None => None };
                let im = crate::soap::import(&ctx.http, &s, &src, text).await?;
                return crate::soap::save(&ctx, im, Some(old));
            }
            let sp = old.spec.clone().ok_or_else(|| ApiError::bad(format!("{s} was not made from a spec; import_spec makes a new service")))?;
            let src = a.str("source").unwrap_or(sp.source.clone());
            let text = match a.str("spec") {
                Some(t) => t,
                None if src.is_empty() => std::fs::read_to_string(folder(&ctx, &s).join(&sp.openapi)).map_err(|_| ApiError::bad("source: the spec's URL or file (this service was imported from text)"))?,
                None => read_source(&ctx, &src, None).await?,
            };
            let im = import(&ctx.http, &s, if src.is_empty() { &sp.openapi } else { &src }, Some(text)).await?;
            save(&ctx, im, Some(old))
        })
        .req("service", "a service made from a spec or a WSDL").param("source", "a new URL or file (default: where it came from)").param("spec", "the text")
        .route("POST", "/plugins/{service}/spec"),
    ]
}

#[cfg(test)]
mod t {
    use super::*;

    const SPEC: &str = r##"
openapi: 3.1.0
info: {title: Example Payments API, version: "1.8"}
servers: [{url: "https://api.payments.example.com"}]
security: [{cc: []}]
components:
  securitySchemes:
    cc: {type: oauth2, flows: {clientCredentials: {tokenUrl: /oauth/token, scopes: {}}}}
  schemas:
    ChargeRequest:
      type: object
      required: [amount, currency]
      properties:
        amount: {type: integer}
        currency: {type: string, enum: [usd, eur]}
        email: {type: string, format: email}
        card: {$ref: "#/components/schemas/Card"}
    Card: {type: object, properties: {number: {type: string}}}
    Error: {type: object, properties: {error: {type: object, properties: {code: {type: string}}}}}
paths:
  /v1/charges:
    post:
      operationId: createCharge
      requestBody: {content: {application/json: {schema: {$ref: "#/components/schemas/ChargeRequest"}}}}
      responses:
        "201":
          description: created
          content: {application/json: {schema: {type: object, properties: {id: {type: string}, amount: {type: integer}, created: {type: string, format: date-time}}}}}
        "402":
          description: declined
          content: {application/json: {example: {error: {code: card_declined}}}}
        422:
          description: invalid
          content: {application/json: {schema: {$ref: "#/components/schemas/Error"}}}
  /v1/charges/{id}:
    get:
      operationId: getCharge
      responses:
        "200": {description: ok, content: {application/json: {example: {id: ch_1}}}}
        "404": {description: none, content: {application/json: {schema: {$ref: "#/components/schemas/Error"}}}}
webhooks:
  charge.succeeded:
    post: {requestBody: {content: {application/json: {example: {type: charge.succeeded}}}}}
"##;

    #[tokio::test]
    async fn spec_to_plugin_and_checks() {
        let im = import(&reqwest::Client::new(), "example-payments", "openapi.yaml", Some(SPEC.into())).await.unwrap();
        let p = &im.plugin;
        assert_eq!(p.service.real_host.first(), "api.payments.example.com");
        assert_eq!(p.service.title, "Example Payments API");
        let r = p.routes.iter().find(|r| r.route == "POST /v1/charges").unwrap();
        assert_eq!((r.operation.as_str(), r.receive.auth.as_str(), r.receive.schema.as_str(), r.receive.invalid.as_str()), ("createCharge", "bearer", "ChargeRequest", "invalid"));
        assert_eq!(r.decide[0].answer, "create_charge");
        let created = p.answers.iter().find(|a| a.name == "create_charge").unwrap();
        assert_eq!(created.status, 201);
        assert!(created.body.contains("{{ body.amount | json }}") && created.body.contains("{{ now() }}"), "{}", created.body);
        assert!(p.answers.iter().any(|a| a.name == "payment_required" && a.body.contains("card_declined")));
        assert!(p.routes.iter().any(|r| r.route == "POST /oauth/token" && r.token));
        assert!(p.answers.iter().any(|a| a.name == "webhook_charge_succeeded"));
        assert_eq!(im.webhooks, vec!["charge.succeeded"]);
        // not_found is the same on both routes? only getCharge has it
        let t = serde_yaml::to_string(p).unwrap();
        assert!(plugin::validate(&plugin::parse(&t).unwrap(), &t).is_empty(), "{t}");
        let s = &p.spec.as_ref().unwrap().schemas["ChargeRequest"];
        assert!(check(s, &json!({"amount": 4500, "currency": "usd"}), "", false).is_ok());
        assert_eq!(check(s, &json!({"amount": "45.00", "currency": "usd"}), "", false).unwrap_err(), "amount: expected integer, got \"45.00\"");
        assert_eq!(check(s, &json!({"amount": 1}), "", false).unwrap_err(), "currency is required");
        assert!(check(s, &json!({"amount": 1, "currency": "brl"}), "", false).unwrap_err().contains("is not one of"));
        assert!(check(s, &json!({"amount": 1, "currency": "usd", "email": "nope"}), "", false).unwrap_err().contains("is not a email"));
        assert!(check(s, &json!({"amount": "12", "currency": "usd"}), "", true).is_ok());
        // a created answer renders to JSON with the echoed amount
        let mut sc = Map::new();
        sc.insert("body".into(), json!({"amount": 4500}));
        let out = crate::expr::render(&created.body, &sc).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["amount"], 4500);
        assert!(v["id"].as_str().unwrap().starts_with("id_") || v["id"].as_str().unwrap().len() > 3, "{out}");
    }
}
