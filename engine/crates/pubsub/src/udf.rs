//! Single message transforms: JavaScript UDFs run with QuickJS (rquickjs), with Pub/Sub's signature
//! `function f(message, metadata)`: message {data: string, attributes: {k: string}}, metadata
//! {message_id, publish_time, ordering_key}; the function returns the message (changed or not), or
//! null to filter it out. A thrown error is the transform failing. Each call gets a fresh runtime
//! with a memory limit and a 500 ms budget. AI inference transforms send the message (a JSON
//! request, merged under the configured parameters) to Stuntman's AI models simulator and replace the
//! data with {"original_message": …, "model_output": …}, as Pub/Sub's AI Inference SMT does.

use std::time::{Duration, Instant};

use rquickjs::{Coerced, Context, Ctx as Js, Function, Object, Runtime, Value as JsValue};

use crate::{store::Labels, v1::{message_transform::Transform, MessageTransform}};

#[derive(Debug, PartialEq)]
pub enum Out {
    Keep(Vec<u8>, Labels),
    /// The UDF returned null: filtered.
    Drop,
}

pub struct Meta<'a> {
    pub message_id: &'a str,
    pub publish_time: &'a str,
    pub ordering_key: &'a str,
}

fn js_err(cx: &Js, e: rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = e {
        let v = cx.catch();
        if let Some(ex) = v.as_exception() {
            return ex.message().unwrap_or_else(|| "exception".into());
        }
        if let Ok(Coerced(s)) = v.get::<Coerced<String>>() {
            return s;
        }
    }
    e.to_string()
}

/// Runs one UDF on one message.
pub fn run(code: &str, func: &str, data: &[u8], attrs: &Labels, meta: &Meta) -> Result<Out, String> {
    let rt = Runtime::new().map_err(|e| e.to_string())?;
    rt.set_memory_limit(32 << 20);
    let deadline = Instant::now() + Duration::from_millis(500);
    rt.set_interrupt_handler(Some(Box::new(move || Instant::now() > deadline)));
    let cx = Context::full(&rt).map_err(|e| e.to_string())?;
    cx.with(|cx| {
        let e = |x: rquickjs::Error| js_err(&cx, x);
        cx.eval::<(), _>(code).map_err(e)?;
        let f: Function = cx.globals().get(func).map_err(|_| format!("the code defines no function {func}"))?;
        let msg = Object::new(cx.clone()).map_err(e)?;
        msg.set("data", String::from_utf8_lossy(data).to_string()).map_err(e)?;
        let at = Object::new(cx.clone()).map_err(e)?;
        for (k, v) in attrs { at.set(k.as_str(), v.as_str()).map_err(e)?; }
        msg.set("attributes", at).map_err(e)?;
        let md = Object::new(cx.clone()).map_err(e)?;
        md.set("message_id", meta.message_id).map_err(e)?;
        md.set("publish_time", meta.publish_time).map_err(e)?;
        md.set("ordering_key", meta.ordering_key).map_err(e)?;
        let r: JsValue = f.call((msg, md)).map_err(e)?;
        if r.is_null() || r.is_undefined() { return Ok(Out::Drop) }
        let o = r.as_object().ok_or("the UDF must return a message object or null")?;
        let data: Coerced<String> = o.get("data").map_err(|_| "the returned message has no data".to_string())?;
        let mut out = Labels::new();
        if let Ok(a) = o.get::<_, Object>("attributes") {
            for p in a.props::<String, Coerced<String>>() {
                let (k, Coerced(v)) = p.map_err(e)?;
                out.insert(k, v);
            }
        }
        Ok(Out::Keep(data.0.into_bytes(), out))
    })
}

/// One transform that is on: JavaScript (code, function) or AI inference (endpoint, parameters).
#[derive(Clone, Debug)]
pub enum Step {
    Js(String, String),
    Ai(String, serde_json::Value),
    /// Compression transform with zlib: true compresses the data, false decompresses it.
    Zlib(bool),
}

impl Step {
    pub fn name(&self) -> String {
        match self { Step::Js(_, f) => format!("{f} (JavaScript)"), Step::Ai(e, _) => format!("{} (AI inference)", e.rsplit('/').next().unwrap_or(e)),
            Step::Zlib(c) => format!("zlib {}", if *c { "compress" } else { "decompress" }) }
    }
}

fn pb_value(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match &v.kind {
        Some(Kind::NumberValue(n)) => serde_json::json!(n),
        Some(Kind::StringValue(s)) => serde_json::json!(s),
        Some(Kind::BoolValue(b)) => serde_json::json!(b),
        Some(Kind::StructValue(s)) => pb_struct(s),
        Some(Kind::ListValue(l)) => serde_json::Value::Array(l.values.iter().map(pb_value).collect()),
        _ => serde_json::Value::Null,
    }
}

fn pb_struct(s: &prost_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(s.fields.iter().map(|(k, v)| (k.clone(), pb_value(v))).collect())
}

/// The transforms of a topic or subscription that are on, in order.
pub fn steps(t: &[MessageTransform]) -> Vec<Step> {
    t.iter().filter(|x| !x.disabled).filter_map(|x| match &x.transform {
        Some(Transform::JavascriptUdf(u)) => Some(Step::Js(u.code.clone(), u.function_name.clone())),
        Some(Transform::AiInference(a)) => Some(Step::Ai(a.endpoint.clone(), match &a.inference_mode {
            Some(crate::v1::ai_inference::InferenceMode::UnstructuredInference(u)) => u.parameters.as_ref().map(pb_struct).unwrap_or(serde_json::json!({})),
            None => serde_json::json!({}),
        })),
        Some(Transform::Compression(c)) => Some(Step::Zlib(c.compression_mode != 2)),
        None => None,
    }).collect()
}

/// The HTTP status in an AI inference error ("AI inference failed: HTTP 429 …"), 0 for others.
pub fn http_of(e: &str) -> u16 {
    e.split("HTTP ").nth(1).and_then(|x| x.get(..3)).and_then(|x| x.parse().ok()).unwrap_or(0)
}

/// Sends the message to the AI models simulator: the request is the data (a JSON object) with the
/// parameters under it (the message wins). The endpoint's model picks the API: a body with
/// "contents" goes to Gemini generateContent, an Anthropic publisher to Messages, the rest to Chat
/// Completions (OpenAI-compatible, as Gemini on Vertex takes it).
pub async fn infer(ctx: &stuntman_core::Ctx, endpoint: &str, params: &serde_json::Value, data: &[u8]) -> Result<Vec<u8>, String> {
    let req: serde_json::Value = serde_json::from_slice(data).map_err(|_| "AI inference failed: the message data is not a JSON request".to_string())?;
    let mut merged = params.as_object().cloned().unwrap_or_default();
    for (k, v) in req.as_object().cloned().unwrap_or_default() { merged.insert(k, v); }
    let model = endpoint.split("/models/").nth(1).or_else(|| endpoint.split("/endpoints/").nth(1)).unwrap_or("model").to_string();
    let (provider, rest): (&'static str, String) = if merged.contains_key("contents") {
        ("gemini", format!("v1beta/models/{model}:generateContent"))
    } else if endpoint.contains("/publishers/anthropic/") {
        merged.entry("model").or_insert(serde_json::json!(model));
        ("anthropic", "v1/messages".into())
    } else {
        merged.entry("model").or_insert(serde_json::json!(model));
        ("openai", "v1/chat/completions".into())
    };
    let (st, out) = stuntman_ai::model::call(ctx, provider, &rest, "pubsub-ai-inference", &serde_json::Value::Object(merged)).await;
    if !(200..300).contains(&st) {
        let why = out["error"]["message"].as_str().map(String::from).unwrap_or(out.to_string());
        return Err(format!("AI inference failed: HTTP {st} from {endpoint}: {why}"));
    }
    Ok(serde_json::json!({"original_message": req, "model_output": out}).to_string().into_bytes())
}

/// Runs a topic's or subscription's transforms in order; the first null filters.
pub async fn apply(ctx: &stuntman_core::Ctx, steps: &[Step], data: Vec<u8>, attrs: Labels, meta: (String, String, String)) -> Result<Out, String> {
    let (mut d, mut a) = (data, attrs);
    for st in steps {
        match st {
            Step::Js(code, f) => {
                let (code, f, d0, a0, m) = (code.clone(), f.clone(), d.clone(), a.clone(), meta.clone());
                let r = tokio::task::spawn_blocking(move || run(&code, &f, &d0, &a0, &Meta { message_id: &m.0, publish_time: &m.1, ordering_key: &m.2 })).await.map_err(|e| e.to_string())??;
                match r { Out::Keep(nd, na) => { d = nd; a = na; } Out::Drop => return Ok(Out::Drop) }
            }
            Step::Ai(endpoint, params) => d = infer(ctx, endpoint, params, &d).await?,
            Step::Zlib(compress) => {
                use std::io::{Read, Write};
                d = if *compress {
                    let mut e = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
                    e.write_all(&d).map_err(|e| e.to_string())?;
                    e.finish().map_err(|e| e.to_string())?
                } else {
                    let mut out = vec![];
                    flate2::read::ZlibDecoder::new(d.as_slice()).read_to_end(&mut out).map_err(|e| format!("zlib decompress: {e}"))?;
                    out
                };
            }
        }
    }
    Ok(Out::Keep(d, a))
}

/// Checks transforms when a topic or subscription is made: JavaScript that compiles and defines the
/// function; AI inference is not offered.
pub fn check(t: &[MessageTransform]) -> Result<(), String> {
    for x in t {
        match &x.transform {
            Some(Transform::JavascriptUdf(u)) => {
                if u.function_name.is_empty() { return Err("javascript_udf.function_name is required".into()) }
                let rt = Runtime::new().map_err(|e| e.to_string())?;
                let cx = Context::full(&rt).map_err(|e| e.to_string())?;
                cx.with(|cx| -> Result<(), String> {
                    cx.eval::<(), _>(u.code.as_str()).map_err(|x| format!("javascript_udf: {}", js_err(&cx, x)))?;
                    cx.globals().get::<_, Function>(u.function_name.as_str()).map_err(|_| format!("javascript_udf: the code defines no function {}", u.function_name))?;
                    Ok(())
                })?;
            }
            Some(Transform::Compression(c)) => {
                if c.compression_algorithm != 1 { return Err("compression: Stuntman has zlib only (no zstd codec in the tree)".into()) }
            }
            Some(Transform::AiInference(a)) => {
                let p: Vec<&str> = a.endpoint.split('/').collect();
                let ok = p.len() >= 6 && p[0] == "projects" && p[2] == "locations" && (p[4] == "endpoints" || (p[4] == "publishers" && p.get(6) == Some(&"models") && p.len() == 8));
                if !ok { return Err(format!("ai_inference.endpoint: projects/<p>/locations/<l>/endpoints/<e> or …/publishers/<publisher>/models/<model>, got \"{}\"", a.endpoint)) }
            }
            None => return Err("a message transform needs a javascript_udf".into()),
        }
    }
    Ok(())
}



#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn udfs_change_filter_and_fail() {
        let m = Meta { message_id: "7", publish_time: "2026-11-15T12:00:00Z", ordering_key: "SHP-1" };
        let code = r#"function redact(message, metadata) {
            const d = JSON.parse(message.data);
            if (d.status === "TEST") return null;
            if (d.status === "BAD") throw new Error("bad status");
            delete d.email;
            message.data = JSON.stringify(d);
            message.attributes["seen_id"] = metadata.message_id + ":" + metadata.ordering_key;
            return message;
        }"#;
        let a = Labels::from([("region".to_string(), "us-east".to_string())]);
        let out = run(code, "redact", br#"{"shipment":"SHP-1","email":"jane@example.com"}"#, &a, &m).unwrap();
        assert_eq!(out, Out::Keep(br#"{"shipment":"SHP-1"}"#.to_vec(), Labels::from([("region".into(), "us-east".into()), ("seen_id".into(), "7:SHP-1".into())])));
        assert_eq!(run(code, "redact", br#"{"status":"TEST"}"#, &a, &m).unwrap(), Out::Drop);
        assert!(run(code, "redact", br#"{"status":"BAD"}"#, &a, &m).unwrap_err().contains("bad status"));
        assert!(run("function loop(m) { while (true) {} }", "loop", b"x", &a, &m).is_err(), "the time budget stops it");
        let t = |code: &str, f: &str| MessageTransform { transform: Some(Transform::JavascriptUdf(crate::v1::JavaScriptUdf { function_name: f.into(), code: code.into() })), ..Default::default() };
        assert!(check(&[t(code, "redact")]).is_ok());
        assert!(check(&[t("function x( {", "x")]).is_err());
        assert!(check(&[t(code, "missing")]).is_err());
    }
}
