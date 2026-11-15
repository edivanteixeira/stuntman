//! SOAP services (A7). A plugin with a `soap:` block takes envelopes on its endpoint: the
//! SOAPAction (or the body's element) picks the operation, the body is read as JSON for the
//! steps (`body.cargoValue`), checked against the types the WSDL declared and WS-Security, and
//! the named answer goes out inside an envelope — or as a fault. The imported WSDL (and its XSDs)
//! is served back on ?wsdl, so clients generated from it work unchanged.

use std::collections::BTreeMap;

use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

use crate::{
    engine::{self, Outcome, Req},
    expr,
    plugin::{self, Answer, Decide, Fault, Plugin, Route, Soap},
    store,
};

pub const NS11: &str = "http://schemas.xmlsoap.org/soap/envelope/";
pub const NS12: &str = "http://www.w3.org/2003/05/soap-envelope";
const WSDL_SOAP11: &str = "http://schemas.xmlsoap.org/wsdl/soap/";
const WSDL_SOAP12: &str = "http://schemas.xmlsoap.org/wsdl/soap12/";

// ---------------------------------------------------------------- envelopes

/// What came in: the version, the body's element and its content as JSON, the UsernameToken.
#[derive(Debug, Default, Clone)]
pub struct Envelope {
    pub version: String,
    pub element: String,
    pub body: Value,
    pub token: Option<(String, String)>,
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == name)
}

fn kids<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Vec<roxmltree::Node<'a, 'i>> {
    n.children().filter(|c| c.is_element() && c.tag_name().name() == name).collect()
}

fn local(n: &roxmltree::Node) -> String {
    n.tag_name().name().to_string()
}

/// An element as JSON: children by local name (repeated → array), a leaf → its text.
pub fn to_json(n: roxmltree::Node) -> Value {
    let kids: Vec<_> = n.children().filter(|c| c.is_element()).collect();
    if kids.is_empty() {
        if n.attributes().any(|a| a.name() == "nil" && a.value() == "true") {
            return Value::Null;
        }
        return json!(n.text().unwrap_or("").trim());
    }
    let mut m = Map::new();
    for c in kids {
        let (k, v) = (local(&c), to_json(c));
        match m.get_mut(&k) {
            Some(Value::Array(a)) => a.push(v),
            Some(x) => *x = json!([x.clone(), v]),
            None => {
                m.insert(k, v);
            }
        }
    }
    Value::Object(m)
}

pub fn parse_envelope(raw: &[u8]) -> Result<Envelope, String> {
    let t = std::str::from_utf8(raw).map_err(|_| "the body is not UTF-8 text".to_string())?;
    let doc = roxmltree::Document::parse(t.trim_start_matches('\u{feff}')).map_err(|e| format!("not XML: {e}"))?;
    let root = doc.root_element();
    let ns = root.tag_name().namespace().unwrap_or("");
    if local(&root) != "Envelope" || (ns != NS11 && ns != NS12) {
        return Err("not a SOAP envelope (no soap:Envelope)".into());
    }
    let version = if ns == NS11 { "1.1" } else { "1.2" }.to_string();
    let token = child(root, "Header").and_then(|h| child(h, "Security")).and_then(|s| child(s, "UsernameToken")).map(|u| {
        let txt = |n: &str| child(u, n).and_then(|x| x.text()).unwrap_or("").trim().to_string();
        (txt("Username"), txt("Password"))
    });
    let body = child(root, "Body").ok_or("the envelope has no soap:Body")?;
    let first = body.children().find(|c| c.is_element()).ok_or("soap:Body is empty")?;
    Ok(Envelope { version, element: local(&first), body: to_json(first), token })
}

pub fn content_type(version: &str, action: &str) -> String {
    if version == "1.1" {
        "text/xml; charset=utf-8".into()
    } else if action.is_empty() {
        "application/soap+xml; charset=utf-8".into()
    } else {
        format!("application/soap+xml; charset=utf-8; action=\"{action}\"")
    }
}

pub fn envelope(version: &str, payload: &str) -> String {
    let ns = if version == "1.1" { NS11 } else { NS12 };
    format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<soap:Envelope xmlns:soap=\"{ns}\">\n  <soap:Body>\n{}\n  </soap:Body>\n</soap:Envelope>\n", indent(payload.trim(), "    "))
}

fn indent(s: &str, pad: &str) -> String {
    s.lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n")
}

fn esc_json(v: &Value) -> Value {
    match v {
        Value::String(s) => json!(esc(s)),
        Value::Array(a) => Value::Array(a.iter().map(esc_json).collect()),
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), esc_json(x))).collect()),
        x => x.clone(),
    }
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A fault in the version's own shape. code: Sender | Receiver | VersionMismatch | MustUnderstand.
pub fn fault(version: &str, code: &str, reason: &str, detail: &str) -> String {
    let d = detail.trim();
    let payload = if version == "1.1" {
        let c = match code { "Sender" => "Client", "Receiver" => "Server", c => c };
        format!("<soap:Fault>\n  <faultcode>soap:{c}</faultcode>\n  <faultstring>{}</faultstring>{}\n</soap:Fault>", esc(reason), if d.is_empty() { String::new() } else { format!("\n  <detail>\n{}\n  </detail>", indent(d, "    ")) })
    } else {
        let c = match code { "Client" => "Sender", "Server" => "Receiver", c => c };
        format!("<soap:Fault>\n  <soap:Code><soap:Value>soap:{c}</soap:Value></soap:Code>\n  <soap:Reason><soap:Text xml:lang=\"en\">{}</soap:Text></soap:Reason>{}\n</soap:Fault>", esc(reason), if d.is_empty() { String::new() } else { format!("\n  <soap:Detail>\n{}\n  </soap:Detail>", indent(d, "    ")) })
    };
    envelope(version, &payload)
}

/// The SOAPAction a request names: the 1.1 header, or the 1.2 content-type's action parameter.
pub fn action_of(headers: &BTreeMap<String, String>) -> String {
    let h = headers.get("soapaction").cloned().unwrap_or_default();
    let a = if h.trim().is_empty() {
        headers.get("content-type").and_then(|c| c.split(';').find_map(|p| p.trim().strip_prefix("action="))).unwrap_or("").to_string()
    } else {
        h
    };
    a.trim().trim_matches('"').to_string()
}

/// The operation an envelope is for, and how it was picked.
pub fn pick<'a>(p: &'a Plugin, action: &str, element: &str) -> Option<(&'a Route, &'static str)> {
    if !action.is_empty() {
        if let Some(r) = p.operations.iter().find(|r| (!r.action.is_empty() && r.action == action) || action.rsplit(['/', '#', ':']).next() == Some(r.operation.as_str())) {
            return Some((r, "by SOAPAction"));
        }
    }
    p.operations.iter().find(|r| r.input == element || r.operation == element).map(|r| (r, if action.is_empty() { "from the body (no SOAPAction)" } else { "from the body (unknown SOAPAction)" }))
}

// ---------------------------------------------------------------- types

/// "notes?" → (notes, optional, many); "items*" → (items, optional, many).
fn field(k: &str) -> (&str, bool, bool) {
    if let Some(n) = k.strip_suffix('?') {
        (n, true, false)
    } else if let Some(n) = k.strip_suffix('*') {
        (n, true, true)
    } else if let Some(n) = k.strip_suffix('+') {
        (n, false, true)
    } else {
        (k, false, false)
    }
}

fn simple_ok(ty: &str, s: &str) -> bool {
    let s = s.trim();
    if let Some(list) = ty.strip_prefix("enum(").and_then(|l| l.strip_suffix(')')) {
        return list.split(',').any(|v| v.trim() == s);
    }
    match ty {
        "decimal" | "double" | "float" => s.parse::<f64>().is_ok(),
        "int" | "integer" | "long" | "short" | "byte" => s.parse::<i64>().is_ok(),
        "boolean" => ["true", "false", "1", "0"].contains(&s),
        "date" => s.len() >= 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' && s[..4].parse::<u32>().is_ok(),
        "dateTime" => s.len() >= 19 && s.as_bytes()[10] == b'T' && simple_ok("date", &s[..10]),
        _ => true,
    }
}

/// Checks a body against soap.types: missing required fields, values of the wrong simple type,
/// fields the type does not have. ponytail: element order and facets (length, pattern) are not checked.
pub fn check(types: &BTreeMap<String, BTreeMap<String, String>>, ty: &str, v: &Value) -> Result<(), String> {
    let Some(spec) = types.get(ty) else { return Ok(()) };
    let empty = Map::new();
    let obj = v.as_object().unwrap_or(&empty);
    for k in obj.keys() {
        if !spec.keys().any(|s| field(s).0 == k) {
            return Err(format!("{k} is not in the XSD for {ty}"));
        }
    }
    for (k, t) in spec {
        let (name, optional, many) = field(k);
        let items: Vec<&Value> = match obj.get(name) {
            None | Some(Value::Null) if optional => continue,
            None => return Err(format!("{name} is missing")),
            Some(Value::Array(a)) if many => a.iter().collect(),
            Some(Value::Array(_)) => return Err(format!("{name} can appear only once")),
            Some(x) => vec![x],
        };
        for it in items {
            if types.contains_key(t) {
                check(types, t, it)?;
            } else if let Some(s) = it.as_str() {
                if !simple_ok(t, s) {
                    if t.starts_with("enum(") {
                        return Err(format!("{name} “{s}” is not one of {}", &t[5..t.len() - 1]));
                    }
                    return Err(format!("{name} is not a{} {t}", if "aeiou".contains(&t[..1]) { "n" } else { "" }));
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- serving

fn early(o: &mut Outcome, version: &str, code: &str, reason: &str, answer: &str, step: Value) {
    o.status = 500;
    o.headers = vec![("content-type".into(), content_type(version, ""))];
    o.body = fault(version, code, reason, "").into_bytes();
    o.answer = answer.into();
    o.sentence = reason.into();
    o.steps.push(step);
}

/// Answers one envelope: the operation, its steps, then the envelope or fault. `dry`: Try it.
pub async fn serve(ctx: &Ctx, p: &Plugin, rq: &Req, dry: bool) -> (Option<Route>, Outcome) {
    let sp = p.soap.clone().unwrap_or_default();
    let mut o = Outcome::default();
    let want = if sp.version == "both" { "" } else { sp.version.as_str() };
    let env = match parse_envelope(&rq.raw_body) {
        Ok(e) => e,
        Err(e) => {
            let v = if want.is_empty() { "1.2" } else { want };
            early(&mut o, v, "Sender", &format!("Bad request: {e}"), "bad envelope", json!({"step": "receive", "text": e, "ok": false}));
            return (None, o);
        }
    };
    let v = env.version.clone();
    if !want.is_empty() && v != want {
        // A 1.1 client on a 1.2 service gets a 1.1 fault, the only shape it can read.
        early(&mut o, &v, "VersionMismatch", &format!("This service speaks SOAP {want}; the envelope is SOAP {v}"), "version mismatch",
            json!({"step": "receive", "text": format!("SOAP {v} envelope to a SOAP {want} service"), "ok": false}));
        return (None, o);
    }
    let action = action_of(&rq.headers);
    let Some((op, how)) = pick(p, &action, &env.element) else {
        let what = if action.is_empty() { env.element.clone() } else { action.clone() };
        if !dry {
            let req_json = json!({"method": "POST", "path": rq.path, "headers": rq.headers, "body": String::from_utf8_lossy(&rq.raw_body).chars().take(16 * 1024).collect::<String>()});
            let ops: Vec<&str> = p.operations.iter().map(|r| r.operation.as_str()).collect();
            let close = json!({"service": p.service.name, "route": "", "text": format!("Not in the WSDL. Operations: {}.", ops.join(", ")), "score": 0});
            let _ = store::unmatched_add(ctx, "SOAP", &format!("{} {}", rq.path, env.element), &rq.headers.get("host").cloned().unwrap_or_default(), &p.service.name, &rq.caller, req_json, close).await;
        }
        early(&mut o, &v, "Sender", &format!("The operation {what} is not in this service's WSDL"), "no operation",
            json!({"step": "receive", "text": format!("No operation for {what}: listed in Unmatched"), "ok": false}));
        o.headers.push(("x-stuntman".into(), "no route".into()));
        return (None, o);
    };
    let op = op.clone();
    let mut pre = vec![json!({"step": "receive", "text": format!("SOAP {v} envelope · {} {how}", op.operation), "ok": how == "by SOAPAction"})];

    // WS-Security
    let sec = sp.security.as_str();
    if sec == "any" || sec == "username_token" {
        let ok = match (&env.token, sec) {
            (None, _) => false,
            (Some(_), "any") => true,
            (Some((u, pw)), _) => sp.users.get(u).map(|s| util::resolve_secret(s, &ctx.data_dir) == *pw).unwrap_or(false),
        };
        if !ok {
            let why = if env.token.is_none() { "no UsernameToken in the header" } else { "UsernameToken refused" };
            o.steps = pre;
            early(&mut o, &v, "Sender", "The security token could not be authenticated or authorized", "security refused", json!({"step": "receive", "text": format!("WS-Security: {why}"), "ok": false}));
            o.sentence = format!("{} called {}: WS-Security refused ({why})", rq.caller, op.operation);
            return (Some(op), o);
        }
        pre.push(json!({"step": "receive", "text": format!("WS-Security UsernameToken {}", env.token.as_ref().map(|t| t.0.as_str()).unwrap_or("")), "ok": true}));
    }
    // The XSD
    if sp.validate && !op.input.is_empty() {
        if let Err(e) = check(&sp.types, &op.input, &env.body) {
            o.steps = pre;
            early(&mut o, &v, "Sender", &format!("Validation error: {e}"), "XSD fault", json!({"step": "receive", "text": format!("Body breaks the XSD: {e}"), "ok": false}));
            o.sentence = format!("{} called {}: the body broke the XSD ({e})", rq.caller, op.operation);
            return (Some(op), o);
        }
        pre.push(json!({"step": "receive", "text": format!("Body checked against {}", op.input), "ok": true}));
    }

    let mut req = rq.clone();
    // Text goes back into XML answers as it came: escaped, so "Smith & Sons" stays valid XML.
    req.body = esc_json(&env.body);
    o = engine::run(ctx, p, &op, Map::new(), &req, dry).await;
    pre.append(&mut o.steps);
    o.steps = pre;
    // "The app sent": the envelope's first fields (SOAP operations declare no receive.body).
    if o.sent.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        let mut lv = vec![];
        engine::leaves(&env.body, "", &mut lv);
        o.sent = Value::Object(lv.into_iter().collect());
    }
    o.scope.insert("soap_version".into(), json!(v));
    wrap(p, &op, &v, &mut o);
    (Some(op), o)
}

/// Puts what the steps chose inside an envelope, or a fault.
fn wrap(p: &Plugin, op: &Route, v: &str, o: &mut Outcome) {
    if o.network.is_some() {
        return;
    }
    let payload = String::from_utf8_lossy(&o.body).to_string();
    let keep: Vec<(String, String)> = o.headers.iter().filter(|(k, _)| k != "content-type").cloned().collect();
    match p.answers.iter().find(|a| a.name == o.answer) {
        Some(a) => {
            match &a.fault {
                Some(f) => {
                    let reason = expr::render(&f.reason, &o.scope).unwrap_or_else(|_| f.reason.clone());
                    o.body = fault(v, &f.code, &reason, &payload).into_bytes();
                    if a.status == 200 {
                        o.status = 500;
                    }
                }
                None => o.body = envelope(v, &payload).into_bytes(),
            }
            if let Ok(doc) = roxmltree::Document::parse(&payload) {
                let mut lv = vec![];
                engine::leaves(&to_json(doc.root_element()), "", &mut lv);
                o.answered = Value::Object(lv.into_iter().collect());
            }
            if a.sentence.is_empty() {
                o.sentence = format!("{} called {} → {}", o.scope.get("caller").and_then(|c| c.as_str()).unwrap_or("the app"), op.operation, a.name);
            }
        }
        // The generic failures that are faults in SOAP; 401/403/503/html stay HTTP answers.
        None if ["500", "no answer"].contains(&o.answer.as_str()) => {
            o.status = 500;
            o.body = fault(v, "Receiver", "Internal Server Error", "").into_bytes();
        }
        None => return,
    }
    o.headers = keep;
    o.headers.push(("content-type".into(), content_type(v, "")));
}

/// GET ?wsdl, and the XSDs it imports, served from apis/<service>.soap/.
pub fn serve_file(ctx: &Ctx, p: &Plugin, inner: &str, query: &str) -> Option<Response> {
    let sp = p.soap.as_ref()?;
    let dir = folder(ctx, &p.service.name);
    let q = query.to_lowercase();
    let name = if q == "wsdl" || q.starts_with("wsdl&") || q.starts_with("wsdl=") || q == "singlewsdl" {
        sp.wsdl.clone()
    } else if let Some(x) = q.strip_prefix("xsd=") {
        x.to_string()
    } else {
        let last = inner.rsplit('/').next().unwrap_or("");
        if !(last.ends_with(".xsd") || last.ends_with(".wsdl")) {
            return None;
        }
        last.to_string()
    };
    if name.is_empty() || name.contains("..") || name.contains('/') {
        return None;
    }
    let t = std::fs::read_to_string(dir.join(&name)).ok()?;
    Some(([("content-type", "text/xml; charset=utf-8")], t).into_response())
}

pub fn folder(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    plugin::dir(ctx).join(format!("{service}.soap"))
}

/// Posts an envelope to the app, as the provider's callback would. Returns (status, body).
pub async fn post(ctx: &Ctx, url: &str, version: &str, action: &str, payload: &str) -> Result<(u16, String), String> {
    let mut r = ctx.http.post(url).header("content-type", content_type(version, action)).body(envelope(version, payload));
    if version == "1.1" {
        r = r.header("SOAPAction", format!("\"{action}\""));
    }
    let res = r.send().await.map_err(|e| if e.is_connect() { "connection refused".into() } else { e.to_string() })?;
    let s = res.status().as_u16();
    Ok((s, res.text().await.unwrap_or_default()))
}

// ---------------------------------------------------------------- WSDL import

/// A WSDL read: the plugin it makes, the files to keep, and what could not be read.
pub struct Imported {
    pub plugin: Plugin,
    pub files: Vec<(String, String)>,
    pub warnings: Vec<String>,
    pub callbacks: Vec<String>,
}

fn strip(q: &str) -> &str {
    q.rsplit(':').next().unwrap_or(q)
}

pub(crate) fn base_name(loc: &str) -> String {
    loc.split(['?', '#']).next().unwrap_or(loc).rsplit(['/', '\\']).next().unwrap_or(loc).to_string()
}

pub(crate) fn join(base: &str, loc: &str) -> String {
    if loc.contains("://") || loc.starts_with('/') && !base.contains("://") {
        return loc.to_string();
    }
    match base.rfind(['/', '\\']) {
        Some(i) => format!("{}{loc}", &base[..=i]),
        None => loc.to_string(),
    }
}

pub(crate) async fn fetch(http: &reqwest::Client, src: &str) -> Result<String, String> {
    if src.contains("://") {
        let r = http.get(src).send().await.map_err(|e| e.to_string())?;
        if !r.status().is_success() {
            return Err(format!("{} ({})", base_name(src), r.status().as_u16()));
        }
        r.text().await.map_err(|e| e.to_string())
    } else {
        std::fs::read_to_string(src).map_err(|e| format!("{} ({e})", base_name(src)))
    }
}

fn simple(t: &str) -> String {
    match strip(t) {
        "string" | "normalizedString" | "token" | "anyURI" | "ID" | "IDREF" | "NCName" | "Name" | "language" | "QName" => "string",
        "decimal" => "decimal",
        "double" | "float" => "double",
        "int" | "integer" | "long" | "short" | "byte" | "unsignedInt" | "unsignedLong" | "unsignedShort" | "unsignedByte" | "positiveInteger" | "nonNegativeInteger" | "negativeInteger" | "nonPositiveInteger" => "int",
        "boolean" => "boolean",
        "date" => "date",
        "dateTime" => "dateTime",
        "time" => "time",
        "base64Binary" | "hexBinary" => "base64",
        other => other,
    }
    .to_string()
}

/// A type's fields in the XSD's order.
type Fields = Vec<(String, String)>;

#[derive(Default)]
struct Schemas {
    /// element name → type name (registered in `types`)
    elements: BTreeMap<String, String>,
    types: BTreeMap<String, Fields>,
    /// simpleType name → its base simple type
    simples: BTreeMap<String, String>,
    ns: BTreeMap<String, String>,
}

impl Schemas {
    fn read(&mut self, schema: roxmltree::Node) {
        let tns = schema.attribute("targetNamespace").unwrap_or("").to_string();
        for c in schema.children().filter(|c| c.is_element()) {
            let Some(name) = c.attribute("name") else { continue };
            match c.tag_name().name() {
                "element" => {
                    let ty = match c.attribute("type") {
                        Some(t) => strip(t).to_string(),
                        None => {
                            let fields = self.complex(c, name);
                            self.types.insert(name.to_string(), fields);
                            name.to_string()
                        }
                    };
                    self.elements.insert(name.to_string(), ty);
                    self.ns.insert(name.to_string(), tns.clone());
                }
                "complexType" => {
                    let f = self.complex_fields(c, name);
                    self.types.insert(name.to_string(), f);
                }
                "simpleType" => {
                    let vals: Vec<&str> = c.descendants().filter(|d| d.tag_name().name() == "enumeration").filter_map(|d| d.attribute("value")).collect();
                    let base = if vals.is_empty() {
                        c.descendants().find(|d| d.tag_name().name() == "restriction").and_then(|r| r.attribute("base")).map(simple).unwrap_or("string".into())
                    } else {
                        format!("enum({})", vals.join(", "))
                    };
                    self.simples.insert(name.to_string(), base);
                }
                _ => {}
            }
        }
    }

    /// The fields of an element with an inline complexType.
    fn complex(&mut self, el: roxmltree::Node, owner: &str) -> Fields {
        match el.children().find(|c| c.tag_name().name() == "complexType") {
            Some(ct) => self.complex_fields(ct, owner),
            None => vec![],
        }
    }

    fn complex_fields(&mut self, ct: roxmltree::Node, owner: &str) -> Fields {
        let mut out = vec![];
        for d in ct.descendants().filter(|d| d.is_element()) {
            // extension base: the base type's fields come first
            if d.tag_name().name() == "extension" {
                if let Some(b) = d.attribute("base") {
                    out.push((format!("__base:{}", strip(b)), String::new()));
                }
            }
            if d.tag_name().name() != "element" || d == ct {
                continue;
            }
            // only this type's own elements, not those of nested anonymous types
            if d.ancestors().skip(1).take_while(|a| *a != ct).any(|a| a.tag_name().name() == "element") {
                continue;
            }
            let Some(n) = d.attribute("name").or(d.attribute("ref").map(strip)) else { continue };
            let ty = match d.attribute("type") {
                Some(t) => strip(t).to_string(),
                None if d.attribute("ref").is_some() => format!("__ref:{n}"),
                None => {
                    let inner = format!("{owner}.{n}");
                    let f = self.complex(d, &inner);
                    if f.is_empty() {
                        "string".into()
                    } else {
                        self.types.insert(inner.clone(), f);
                        inner
                    }
                }
            };
            let min0 = d.attribute("minOccurs") == Some("0") || d.ancestors().take_while(|a| *a != ct).any(|a| a.tag_name().name() == "choice");
            let many = d.attribute("maxOccurs").map(|m| m == "unbounded" || m.parse::<u32>().map(|x| x > 1).unwrap_or(false)).unwrap_or(false);
            out.push((format!("{n}{}", match (min0, many) { (true, true) => "*", (false, true) => "+", (true, false) => "?", _ => "" }), ty));
        }
        out
    }

    /// Simple types resolved, bases and refs expanded: the `types` block a plugin keeps.
    fn finish(mut self) -> (BTreeMap<String, Fields>, BTreeMap<String, String>, BTreeMap<String, String>) {
        let snapshot = self.types.clone();
        for f in self.types.values_mut() {
            // extension: the base type's fields first, then the type's own
            let mut out: Fields = vec![];
            for (k, v) in f.drain(..) {
                match k.strip_prefix("__base:").and_then(|b| snapshot.get(b)) {
                    Some(bf) => out.extend(bf.iter().filter(|(k, _)| !k.starts_with("__")).cloned()),
                    None if k.starts_with("__base:") => {}
                    None => out.push((k, v)),
                }
            }
            *f = out;
            for (_, v) in f.iter_mut() {
                if let Some(r) = v.strip_prefix("__ref:") {
                    *v = self.elements.get(r).cloned().unwrap_or("string".into());
                }
                if !snapshot.contains_key(v.as_str()) {
                    *v = self.simples.get(v.as_str()).cloned().unwrap_or_else(|| simple(v));
                }
            }
        }
        (self.types, self.elements, self.ns)
    }
}

pub(crate) fn snake(s: &str) -> String {
    let mut o = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 && !o.ends_with('_') {
            o.push('_');
        }
        if c.is_alphanumeric() {
            o.push(c.to_ascii_lowercase());
        } else if !o.ends_with('_') {
            o.push('_');
        }
    }
    o.trim_matches('_').to_string()
}

fn sample(ty: &str, field: &str, input_fields: &[String]) -> String {
    if input_fields.iter().any(|f| f == field) {
        return format!("{{{{ body.{field} }}}}");
    }
    match ty {
        "decimal" | "double" => "1.00".into(),
        "int" => "1".into(),
        "boolean" => "true".into(),
        "date" => "2026-01-01".into(),
        "dateTime" => "{{ now() }}".into(),
        t if t.starts_with("enum(") => t[5..t.len() - 1].split(',').next().unwrap_or("").trim().to_string(),
        _ => format!("{}-000123", field.chars().filter(|c| c.is_ascii_alphabetic()).take(3).collect::<String>().to_uppercase()),
    }
}

/// An element filled with sample values: the default answer of an operation.
fn sample_xml(types: &BTreeMap<String, Fields>, ty: &str, name: &str, ns: Option<&str>, input: &[String], depth: usize, pad: &str) -> String {
    let xmlns = ns.filter(|n| !n.is_empty()).map(|n| format!(" xmlns=\"{n}\"")).unwrap_or_default();
    let Some(f) = types.get(ty).filter(|_| depth < 4) else {
        return format!("{pad}<{name}{xmlns}>{}</{name}>", sample(ty, name, input));
    };
    let inner: Vec<String> = f.iter().filter(|(k, _)| !k.ends_with('?') && !k.ends_with('*')).map(|(k, t)| sample_xml(types, t, field(k).0, None, input, depth + 1, &format!("{pad}  "))).collect();
    if inner.is_empty() {
        return format!("{pad}<{name}{xmlns}/>");
    }
    format!("{pad}<{name}{xmlns}>\n{}\n{pad}</{name}>", inner.join("\n"))
}

/// Reads a WSDL (URL, file path, or its text) and the XSDs it imports into a plugin.
pub async fn import(http: &reqwest::Client, service: &str, source: &str, text: Option<String>) -> Result<Imported, ApiError> {
    plugin::valid_name(service)?;
    let wsdl = match text {
        Some(t) => t,
        None => fetch(http, source).await.map_err(|e| ApiError::bad(format!("Could not read the WSDL: {e}")))?,
    };
    let wname = { let b = base_name(source); if b.is_empty() || !b.contains('.') { format!("{service}.wsdl") } else { b } };
    let mut files = vec![];
    let mut warnings = vec![];
    let mut sch = Schemas::default();
    // Every schema: inline ones and those imported (followed one level deep at a time).
    let mut queue: Vec<(String, String)> = vec![(source.to_string(), wsdl.clone())];
    let mut seen = std::collections::HashSet::new();
    while let Some((loc, txt)) = queue.pop() {
        let doc = roxmltree::Document::parse(&txt).map_err(|e| ApiError::bad(format!("{}: not XML ({e})", base_name(&loc))))?;
        let mut kept = txt.clone();
        for s in doc.descendants().filter(|n| n.tag_name().name() == "schema") {
            sch.read(s);
        }
        for imp in doc.descendants().filter(|n| matches!(n.tag_name().name(), "import" | "include")) {
            let Some(l) = imp.attribute("schemaLocation").or(imp.attribute("location")) else { continue };
            let full = join(&loc, l);
            kept = kept.replace(&format!("\"{l}\""), &format!("\"{}\"", base_name(l)));
            if !seen.insert(full.clone()) {
                continue;
            }
            match fetch(http, &full).await {
                Ok(t) => queue.push((full, t)),
                Err(e) => warnings.push(format!("{e} could not be read: its types answer as empty elements")),
            }
        }
        files.push((if loc == source { wname.clone() } else { base_name(&loc) }, kept));
    }
    let (types, elements, nss) = sch.finish();
    let doc = roxmltree::Document::parse(&wsdl).map_err(|e| ApiError::bad(format!("not XML ({e})")))?;
    let root = doc.root_element();
    if root.tag_name().name() != "definitions" {
        return Err(ApiError::bad(if root.tag_name().name() == "description" { "This is a WSDL 2.0 file; Stuntman reads WSDL 1.1".to_string() } else { "Not a WSDL (no wsdl:definitions)".to_string() }));
    }
    let tns = root.attribute("targetNamespace").unwrap_or("").to_string();
    // message → element
    let mut messages = BTreeMap::new();
    for m in kids(root, "message") {
        let el = kids(m, "part").first().and_then(|p| p.attribute("element").or(p.attribute("type"))).map(strip).unwrap_or("").to_string();
        messages.insert(m.attribute("name").unwrap_or("").to_string(), el);
    }
    // binding: soapAction per operation, and the version
    let bindings = kids(root, "binding");
    let is12 = |b: &roxmltree::Node| b.children().any(|c| c.tag_name().namespace() == Some(WSDL_SOAP12));
    let is11 = |b: &roxmltree::Node| b.children().any(|c| c.tag_name().namespace() == Some(WSDL_SOAP11));
    let (has11, has12) = (bindings.iter().any(is11), bindings.iter().any(is12));
    let version = match (has11, has12) { (true, true) => "both", (true, false) => "1.1", _ => "1.2" };
    let mut actions = BTreeMap::new();
    for b in &bindings {
        for o in kids(*b, "operation") {
            if let Some(a) = o.children().find(|c| c.tag_name().name() == "operation").and_then(|c| c.attribute("soapAction")) {
                actions.entry(o.attribute("name").unwrap_or("").to_string()).or_insert(a.to_string());
            }
        }
    }
    // service address → endpoint and host
    let address = root.descendants().find(|n| n.tag_name().name() == "address" && n.attribute("location").is_some()).and_then(|n| n.attribute("location")).unwrap_or("").to_string();
    let (host, endpoint) = match address.split_once("://") {
        Some((_, rest)) => {
            let (h, p) = rest.split_once('/').unwrap_or((rest, ""));
            (h.split(':').next().unwrap_or("").to_string(), format!("/{}", p.split('?').next().unwrap_or("")))
        }
        None => (String::new(), "/".to_string()),
    };
    // operations; one-way ones without an input in the binding the app calls are callbacks
    let mut ops = vec![];
    let mut answers = vec![];
    let mut callbacks = vec![];
    let mut reach = vec![];
    for pt in kids(root, "portType") {
        for o in kids(pt, "operation") {
            let name = o.attribute("name").unwrap_or("").to_string();
            let msg = |kind: &'static str| kids(o, kind).first().and_then(|x| x.attribute("message")).map(strip).and_then(|m| messages.get(m)).cloned().unwrap_or_default();
            let (input, output) = (msg("input"), msg("output"));
            if input.is_empty() && !output.is_empty() {
                callbacks.push(name);
                continue;
            }
            if ops.iter().any(|r: &Route| r.operation == name) {
                continue;
            }
            let in_ty = elements.get(&input).cloned().unwrap_or(input.clone());
            let out_ty = elements.get(&output).cloned().unwrap_or(output.clone());
            let in_fields: Vec<String> = types.get(&in_ty).map(|f| f.iter().map(|(k, _)| field(k).0.to_string()).collect()).unwrap_or_default();
            reach.push(in_ty.clone());
            reach.push(out_ty.clone());
            let ok = snake(&name);
            answers.push(Answer { name: ok.clone(), status: 200, body: if output.is_empty() { String::new() } else { sample_xml(&types, &out_ty, &output, nss.get(&output).map(String::as_str).or(Some(&tns)), &in_fields, 0, "") },
                note: format!("{name}: the default answer, from {}", if output.is_empty() { "nothing (one-way)".into() } else { output.clone() }), ..Default::default() });
            for f in kids(o, "fault") {
                let fname = f.attribute("name").unwrap_or("Fault").to_string();
                let fel = f.attribute("message").map(strip).and_then(|m| messages.get(m)).cloned().unwrap_or_default();
                let fty = elements.get(&fel).cloned().unwrap_or(fel.clone());
                reach.push(fty.clone());
                let n = snake(&fname);
                if !answers.iter().any(|a| a.name == n) {
                    let words = snake(&fname).replace('_', " ");
                    answers.push(Answer { name: n, status: 500, fault: Some(Fault { code: "Sender".into(), reason: format!("{}{}", words[..1].to_uppercase(), &words[1..]) }),
                        body: if fel.is_empty() { String::new() } else { sample_xml(&types, &fty, &fel, nss.get(&fel).map(String::as_str).or(Some(&tns)), &in_fields, 0, "") },
                        note: format!("the {fname} fault the WSDL declares"), ..Default::default() });
                }
            }
            ops.push(Route { operation: name.clone(), action: actions.get(&name).cloned().unwrap_or_default(), input: in_ty, output: out_ty, decide: vec![Decide { when: None, answer: ok, ..Default::default() }], ..Default::default() });
        }
    }
    if ops.is_empty() {
        return Err(ApiError::bad("The WSDL has no operations the app can call"));
    }
    // keep only the types the operations reach
    let mut keep = BTreeMap::new();
    while let Some(t) = reach.pop() {
        if keep.contains_key(&t) {
            continue;
        }
        if let Some(f) = types.get(&t) {
            reach.extend(f.iter().map(|x| x.1.clone()));
            keep.insert(t, f.iter().cloned().collect::<BTreeMap<_, _>>());
        }
    }
    // "ExampleInsurance" → "Example Insurance"
    let raw = root.attribute("name").or(kids(root, "service").first().and_then(|s| s.attribute("name"))).unwrap_or(service);
    let mut title = String::new();
    for (i, c) in raw.chars().enumerate() {
        if i > 0 && c.is_uppercase() && raw.chars().nth(i - 1).map(|p| p.is_lowercase()).unwrap_or(false) {
            title.push(' ');
        }
        title.push(if c == '_' || c == '-' { ' ' } else { c });
    }
    let p = Plugin {
        service: plugin::Service { name: service.into(), title, origin: "yours".into(), base_url: if host.is_empty() { String::new() } else { format!("https://{host}") }, real_host: host, ..Default::default() },
        soap: Some(Soap { version: version.into(), endpoint, wsdl: wname, source: if source.contains("://") { source.into() } else { String::new() }, validate: true, types: keep, namespace: tns, ..Default::default() }),
        operations: ops,
        answers,
        ..Default::default()
    };
    Ok(Imported { plugin: p, files, warnings, callbacks })
}

/// Saves an import: the plugin YAML and the WSDL files. `merge` keeps the steps, answers and
/// settings of an existing service (re-import).
pub fn save(ctx: &Ctx, mut im: Imported, merge: Option<Plugin>) -> Result<Value, ApiError> {
    let mut added = vec![];
    let mut removed = vec![];
    if let Some(old) = merge {
        let new_ops = std::mem::take(&mut im.plugin.operations);
        for mut r in new_ops {
            match old.operations.iter().find(|o| o.operation == r.operation) {
                Some(o) => {
                    let (a, i, out) = (r.action.clone(), r.input.clone(), r.output.clone());
                    r = o.clone();
                    (r.action, r.input, r.output) = (a, i, out);
                }
                None => added.push(r.operation.clone()),
            }
            im.plugin.operations.push(r);
        }
        removed = old.operations.iter().filter(|o| !im.plugin.operations.iter().any(|n| n.operation == o.operation)).map(|o| o.operation.clone()).collect();
        let fresh = std::mem::take(&mut im.plugin.answers);
        im.plugin.answers = old.answers.clone();
        for a in fresh {
            if !im.plugin.answers.iter().any(|x| x.name == a.name) {
                im.plugin.answers.push(a);
            }
        }
        if let (Some(n), Some(o)) = (im.plugin.soap.as_mut(), old.soap.as_ref()) {
            (n.validate, n.security, n.users) = (o.validate, o.security.clone(), o.users.clone());
        }
        im.plugin.service = old.service.clone();
        im.plugin.data = old.data.clone();
        im.plugin.routes = old.routes.clone();
    }
    let name = im.plugin.service.name.clone();
    let dir = folder(ctx, &name);
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
    for (f, t) in &im.files {
        std::fs::write(dir.join(f), t).map_err(|e| ApiError::internal(e.to_string()))?;
    }
    let v = plugin::save(ctx, &im.plugin)?;
    let p = &im.plugin;
    Ok(json!({"service": name, "version": v, "soap": p.soap.as_ref().map(|s| s.version.clone()), "endpoint": p.soap.as_ref().map(|s| s.endpoint.clone()),
        "operations": p.operations.iter().map(|o| json!({"operation": o.operation, "action": o.action, "input": o.input, "output": o.output})).collect::<Vec<_>>(),
        "faults": p.answers.iter().filter(|a| a.fault.is_some()).map(|a| a.name.clone()).collect::<Vec<_>>(),
        "callbacks": im.callbacks, "warnings": im.warnings, "added": added, "removed": removed, "file": plugin::rel(&name)}))
}

// ---------------------------------------------------------------- tools (A7 "API & MCP")

fn soap_plugin(ctx: &Ctx, s: &str) -> Result<Plugin, ApiError> {
    let p = plugin::get(ctx, s)?;
    if p.soap.is_none() {
        return Err(ApiError::bad(format!("{s} is not a SOAP service (no soap: block)")));
    }
    Ok(p)
}

fn op_view(r: &Route) -> Value {
    json!({"operation": r.operation, "action": r.action, "input": r.input, "output": r.output, "billable": r.billable,
        "answers": r.decide.iter().map(|d| d.answer.clone()).collect::<Vec<_>>(), "steps": r})
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("import_wsdl", "A SOAP service from the provider's WSDL (a URL, a file path, or its text): operations with their SOAPActions, types for checking bodies, a default answer per operation and the faults as named answers. The WSDL is served back on ?wsdl.", |ctx, a| async move {
            let s = a.need("service")?;
            if plugin::file(&ctx, &s).exists() {
                return Err(ApiError::new(409, format!("{s} is already installed; reimport_wsdl updates it")));
            }
            let src = a.str("source").unwrap_or_default();
            if src.is_empty() && a.str("wsdl").is_none() {
                return Err(ApiError::bad("source (a URL or a file) or wsdl (the text) is required"));
            }
            let mut im = import(&ctx.http, &s, &src, a.str("wsdl")).await?;
            if let Some(t) = a.str("title") {
                im.plugin.service.title = t;
            }
            if let Some(h) = a.str("real_host") {
                im.plugin.service.real_host = h;
            }
            save(&ctx, im, None)
        })
        .req("service", "the new service's name, e.g. example-insurance").param("source", "the WSDL's URL or file path").param("wsdl", "the WSDL text, instead of source").param("title", "shown name").param("real_host", "the provider's host (default: from soap:address)")
        .route("POST", "/plugins/wsdl"),
        Tool::new("reimport_wsdl", "Reads the WSDL again (or a new one): new operations start with their default answer; existing ones keep their steps; answers, data, security and service settings stay. Lists what was added and removed.", |ctx, a| async move {
            let s = a.need("service")?;
            let old = soap_plugin(&ctx, &s)?;
            let src = a.str("source").unwrap_or_else(|| old.soap.as_ref().map(|x| x.source.clone()).unwrap_or_default());
            let text = match a.str("wsdl") {
                Some(t) => Some(t),
                None if src.is_empty() => Some(std::fs::read_to_string(folder(&ctx, &s).join(&old.soap.as_ref().unwrap().wsdl)).map_err(|_| ApiError::bad("source: the WSDL's URL or file (this service was imported from text)"))?),
                None => None,
            };
            let im = import(&ctx.http, &s, &src, text).await?;
            save(&ctx, im, Some(old))
        })
        .req("service", "the SOAP service").param("source", "a new URL or file (default: where it came from)").param("wsdl", "the WSDL text")
        .route("POST", "/plugins/{service}/wsdl"),
        Tool::new("list_operations", "A SOAP service's operations: SOAPAction, request and response elements, the answers each can give; plus the version, endpoint and the URL of the served WSDL.", |ctx, a| async move {
            let s = a.need("service")?;
            let p = soap_plugin(&ctx, &s)?;
            let sp = p.soap.clone().unwrap_or_default();
            Ok(json!({"service": s, "version": sp.version, "endpoint": sp.endpoint, "wsdl": sp.wsdl, "validate": sp.validate, "security": sp.security,
                "wsdlUrl": format!("http://127.0.0.1:{}{}/{}{}?wsdl", crate::port(), ctx.prefix(), s, sp.endpoint),
                "operations": p.operations.iter().map(op_view).collect::<Vec<_>>(), "types": sp.types}))
        })
        .req("service", "the SOAP service")
        .route("GET", "/plugins/{service}/operations"),
        Tool::new("set_operation", "Replaces an operation's steps (receive, let, keep, wait, decide, respond, send, fail, billable), like set_route for SOAP; its SOAPAction and types stay as the WSDL says. Validated before saving.", |ctx, a| async move {
            let s = a.need("service")?;
            let mut p = soap_plugin(&ctx, &s)?;
            let name = a.need("operation")?;
            let i = p.operations.iter().position(|r| r.operation == name).ok_or_else(|| ApiError::not_found(format!("No operation {name} in {s}; reimport_wsdl adds new ones")))?;
            let mut r: Route = serde_json::from_value(a.get("steps").cloned().unwrap_or(json!({}))).map_err(|e| ApiError::bad(format!("steps: {e}")))?;
            let cur = &p.operations[i];
            (r.operation, r.route) = (name.clone(), String::new());
            if r.action.is_empty() { r.action = cur.action.clone(); }
            if r.input.is_empty() { r.input = cur.input.clone(); }
            if r.output.is_empty() { r.output = cur.output.clone(); }
            p.operations[i] = r.clone();
            let v = plugin::save(&ctx, &p)?;
            Ok(json!({"operation": op_view(&r), "version": v}))
        })
        .req("service", "the SOAP service").req("operation", "its name in the WSDL").param("steps", "{receive, let, keep, wait, decide, respond, send, fail, billable}")
        .route("PUT", "/plugins/{service}/operations"),
        Tool::new("try_operation", "Runs a SOAP service on an envelope (or an operation and its body XML): the operation it picks, the XSD and WS-Security checks, the steps and the envelope it answers. Nothing is kept, counted, billed or sent.", |ctx, a| async move {
            let s = a.need("service")?;
            let p = soap_plugin(&ctx, &s)?;
            let sp = p.soap.clone().unwrap_or_default();
            let v = if sp.version == "1.1" { "1.1" } else { "1.2" };
            let op = a.str("operation").unwrap_or_default();
            let raw = match a.str("envelope") {
                Some(e) => e,
                None => envelope(v, &a.str("body").ok_or_else(|| ApiError::bad("envelope, or operation and body (the request element's XML)"))?),
            };
            let action = p.operations.iter().find(|r| r.operation == op).map(|r| r.action.clone()).unwrap_or_else(|| a.str("action").unwrap_or_default());
            let mut headers = BTreeMap::new();
            headers.insert("content-type".to_string(), content_type(v, ""));
            if !action.is_empty() {
                headers.insert("soapaction".to_string(), action);
            }
            let rq = Req { method: "POST".into(), path: sp.endpoint.clone(), headers, raw_body: raw.into_bytes(), caller: "try it".into(), ..Default::default() };
            let (r, o) = serve(&ctx, &p, &rq, true).await;
            Ok(json!({"operation": r.map(|r| r.operation), "status": o.status, "answer": o.answer, "sentence": o.sentence, "steps": o.steps,
                "headers": o.headers.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(), "body": String::from_utf8_lossy(&o.body), "note": "Nothing is stored and nothing is billed."}))
        })
        .req("service", "the SOAP service").param("envelope", "the whole envelope the app sends").param("operation", "with body: the operation").param("body", "with operation: the request element's XML").param("action", "a SOAPAction to send")
        .route("POST", "/plugins/{service}/operations/try"),
        Tool::new("send_callback", "Calls the app's endpoint as the provider would: an envelope with this operation's SOAPAction and body (the payload XML). Returns the app's status and answer.", |ctx, a| async move {
            let s = a.need("service")?;
            let p = soap_plugin(&ctx, &s)?;
            let sp = p.soap.clone().unwrap_or_default();
            let v = a.str("version").unwrap_or(if sp.version == "1.1" { "1.1".into() } else { "1.2".into() });
            let (url, op, body) = (a.need("url")?, a.need("operation")?, a.need("body")?);
            let t0 = std::time::Instant::now();
            let (status, answer) = post(&ctx, &url, &v, &op, &body).await.map_err(|e| ApiError::new(502, format!("The app's endpoint: {e}")))?;
            ctx.live.send("apis.callback", &s, &op);
            Ok(json!({"url": url, "operation": op, "status": status, "answer": answer.chars().take(16 * 1024).collect::<String>(), "ms": t0.elapsed().as_millis() as i64}))
        })
        .req("service", "the SOAP service").req("url", "the app's callback endpoint").req("operation", "the SOAPAction (operation name)").req("body", "the payload XML").param("version", "1.1 | 1.2")
        .route("POST", "/plugins/{service}/callback"),
    ]
}

#[cfg(test)]
mod t {
    use super::*;

    const WSDL: &str = r#"<?xml version="1.0"?>
<wsdl:definitions name="PolicyService" targetNamespace="urn:example:insurance" xmlns:wsdl="http://schemas.xmlsoap.org/wsdl/" xmlns:soap12="http://schemas.xmlsoap.org/wsdl/soap12/" xmlns:tns="urn:example:insurance" xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <wsdl:types>
    <xs:schema targetNamespace="urn:example:insurance" elementFormDefault="qualified">
      <xs:element name="RegisterShipmentRequest"><xs:complexType><xs:sequence>
        <xs:element name="shipmentNumber" type="xs:string"/>
        <xs:element name="cargoValue" type="xs:decimal"/>
        <xs:element name="insurer" type="xs:string"/>
        <xs:element name="notes" type="xs:string" minOccurs="0"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="RegisterShipmentResponse"><xs:complexType><xs:sequence>
        <xs:element name="protocol" type="xs:string"/>
        <xs:element name="insurer" type="xs:string"/>
      </xs:sequence></xs:complexType></xs:element>
      <xs:element name="CoverageExceeded"><xs:complexType><xs:sequence><xs:element name="limit" type="xs:decimal"/></xs:sequence></xs:complexType></xs:element>
    </xs:schema>
  </wsdl:types>
  <wsdl:message name="RegisterIn"><wsdl:part name="p" element="tns:RegisterShipmentRequest"/></wsdl:message>
  <wsdl:message name="RegisterOut"><wsdl:part name="p" element="tns:RegisterShipmentResponse"/></wsdl:message>
  <wsdl:message name="CoverageFault"><wsdl:part name="p" element="tns:CoverageExceeded"/></wsdl:message>
  <wsdl:portType name="PolicyPort">
    <wsdl:operation name="RegisterShipment"><wsdl:input message="tns:RegisterIn"/><wsdl:output message="tns:RegisterOut"/><wsdl:fault name="CoverageExceeded" message="tns:CoverageFault"/></wsdl:operation>
  </wsdl:portType>
  <wsdl:binding name="PolicyBinding" type="tns:PolicyPort"><soap12:binding transport="http://schemas.xmlsoap.org/soap/http"/>
    <wsdl:operation name="RegisterShipment"><soap12:operation soapAction="urn:example:insurance/RegisterShipment"/></wsdl:operation>
  </wsdl:binding>
  <wsdl:service name="PolicyService"><wsdl:port name="p" binding="tns:PolicyBinding"><soap12:address location="https://insurance.example.com/PolicyService"/></wsdl:port></wsdl:service>
</wsdl:definitions>"#;

    fn env(body: &str) -> Vec<u8> {
        format!(r#"<soap:Envelope xmlns:soap="{NS12}"><soap:Header><wsse:Security xmlns:wsse="x"><wsse:UsernameToken><wsse:Username>acme</wsse:Username><wsse:Password>pw</wsse:Password></wsse:UsernameToken></wsse:Security></soap:Header><soap:Body>{body}</soap:Body></soap:Envelope>"#).into_bytes()
    }

    #[tokio::test]
    async fn wsdl_to_plugin_and_checks() {
        let im = import(&reqwest::Client::new(), "example-insurance", "PolicyService.wsdl", Some(WSDL.into())).await.unwrap();
        let p = &im.plugin;
        let sp = p.soap.as_ref().unwrap();
        assert_eq!((sp.version.as_str(), sp.endpoint.as_str(), p.service.real_host.as_str()), ("1.2", "/PolicyService", "insurance.example.com"));
        let op = &p.operations[0];
        assert_eq!((op.operation.as_str(), op.action.as_str(), op.input.as_str()), ("RegisterShipment", "urn:example:insurance/RegisterShipment", "RegisterShipmentRequest"));
        assert_eq!(sp.types["RegisterShipmentRequest"]["cargoValue"], "decimal");
        assert!(sp.types["RegisterShipmentRequest"].contains_key("notes?"));
        assert!(p.answers.iter().any(|a| a.name == "coverage_exceeded" && a.fault.is_some()));
        // the default answer echoes the insurer the app sent
        assert!(p.answers[0].body.contains("{{ body.insurer }}"), "{}", p.answers[0].body);
        let t = serde_yaml::to_string(p).unwrap();
        assert!(plugin::validate(&plugin::parse(&t).unwrap(), &t).is_empty(), "{t}");

        let e = parse_envelope(&env("<r:RegisterShipmentRequest xmlns:r='urn:example:insurance'><r:shipmentNumber>SHP-1</r:shipmentNumber><r:cargoValue>12.5</r:cargoValue><r:insurer>globex</r:insurer></r:RegisterShipmentRequest>")).unwrap();
        assert_eq!((e.version.as_str(), e.element.as_str()), ("1.2", "RegisterShipmentRequest"));
        assert_eq!(e.token, Some(("acme".into(), "pw".into())));
        assert_eq!(e.body["cargoValue"], "12.5");
        assert!(check(&sp.types, "RegisterShipmentRequest", &e.body).is_ok());
        let bad = json!({"shipmentNumber": "x", "cargoValue": "lots", "insurer": "g"});
        assert_eq!(check(&sp.types, "RegisterShipmentRequest", &bad).unwrap_err(), "cargoValue is not a decimal");
        assert_eq!(check(&sp.types, "RegisterShipmentRequest", &json!({"cargoValue": "1", "insurer": "g"})).unwrap_err(), "shipmentNumber is missing");
        assert_eq!(pick(p, "", "RegisterShipmentRequest").unwrap().1, "from the body (no SOAPAction)");
        assert_eq!(pick(p, "urn:example:insurance/RegisterShipment", "").unwrap().1, "by SOAPAction");
        let f = fault("1.1", "Sender", "a < b", "");
        assert!(f.contains("<faultcode>soap:Client</faultcode>") && f.contains("<faultstring>a &lt; b</faultstring>") && f.contains(NS11), "{f}");
    }
}
