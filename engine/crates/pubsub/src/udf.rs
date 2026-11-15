//! Single message transforms: JavaScript UDFs run with QuickJS (rquickjs), with Pub/Sub's signature
//! `function f(message, metadata)`: message {data: string, attributes: {k: string}}, metadata
//! {message_id, publish_time, ordering_key}; the function returns the message (changed or not), or
//! null to filter it out. A thrown error is the transform failing. Each call gets a fresh runtime
//! with a memory limit and a 500 ms budget.

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

/// The JavaScript UDFs of a topic or subscription that are on.
pub fn udfs(t: &[MessageTransform]) -> Vec<(String, String)> {
    t.iter().filter(|x| !x.disabled).filter_map(|x| match &x.transform { Some(Transform::JavascriptUdf(u)) => Some((u.code.clone(), u.function_name.clone())), _ => None }).collect()
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
            Some(_) => return Err("Stuntman runs JavaScript UDF transforms only; AI inference transforms are not offered.".into()),
            None => return Err("a message transform needs a javascript_udf".into()),
        }
    }
    Ok(())
}

/// Runs a chain of UDFs in order; the first null filters.
pub fn chain(udfs: &[(String, String)], data: Vec<u8>, attrs: Labels, meta: &Meta) -> Result<Out, String> {
    let (mut d, mut a) = (data, attrs);
    for (code, f) in udfs {
        match run(code, f, &d, &a, meta)? {
            Out::Keep(nd, na) => { d = nd; a = na; }
            Out::Drop => return Ok(Out::Drop),
        }
    }
    Ok(Out::Keep(d, a))
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
