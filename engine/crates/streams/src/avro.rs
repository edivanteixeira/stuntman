//! A small Avro binary codec (JSON in, JSON out) for "produce a message as the partner would, with
//! its schema" and to show what Avro records hold: primitives, records, enums, arrays, maps, unions
//! and fixed; logical types read as their base type. Confluent framing: 0, the schema id (4 bytes),
//! the Avro body.

use std::collections::HashMap;

use serde_json::{json, Map, Value};

use crate::wire::{R, W};

fn named<'a>(names: &'a HashMap<String, Value>, s: &'a Value) -> &'a Value {
    if let Value::String(n) = s {
        if let Some(d) = names.get(n).or_else(|| names.iter().find(|(k, _)| k.rsplit('.').next() == Some(n.as_str())).map(|x| x.1)) {
            return d;
        }
    }
    s
}

fn collect(s: &Value, out: &mut HashMap<String, Value>) {
    match s {
        Value::Object(o) => {
            if let (Some(n), Some(t)) = (o.get("name").and_then(|v| v.as_str()), o.get("type").and_then(|v| v.as_str())) {
                if ["record", "enum", "fixed"].contains(&t) {
                    let full = match o.get("namespace").and_then(|v| v.as_str()) { Some(ns) if !n.contains('.') => format!("{ns}.{n}"), _ => n.to_string() };
                    out.insert(full, s.clone());
                }
            }
            for v in o.values() { collect(v, out); }
        }
        Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
        _ => {}
    }
}

fn type_name(s: &Value) -> String {
    match s {
        Value::String(t) => t.clone(),
        Value::Object(o) => o.get("type").map(type_name).unwrap_or_default(),
        Value::Array(_) => "union".into(),
        _ => String::new(),
    }
}

fn fits(names: &HashMap<String, Value>, s: &Value, v: &Value) -> bool {
    let s = named(names, s);
    match (type_name(s).as_str(), v) {
        ("null", Value::Null) => true,
        ("boolean", Value::Bool(_)) => true,
        ("int" | "long", Value::Number(n)) => n.is_i64(),
        ("float" | "double", Value::Number(_)) => true,
        ("string" | "bytes" | "enum" | "fixed", Value::String(_)) => true,
        ("array", Value::Array(_)) => true,
        ("record" | "map", Value::Object(_)) => true,
        _ => false,
    }
}

pub fn encode(schema: &Value, v: &Value) -> Result<Vec<u8>, String> {
    let mut names = HashMap::new();
    collect(schema, &mut names);
    let mut w = W::default();
    enc(&names, schema, v, &mut w, "")?;
    Ok(w.0)
}

fn enc(names: &HashMap<String, Value>, s: &Value, v: &Value, w: &mut W, path: &str) -> Result<(), String> {
    let s = named(names, s);
    let bad = |want: &str| Err(format!("{}: expected {want}, got {v}", if path.is_empty() { "value" } else { path }));
    match s {
        Value::Array(branches) => {
            // Avro JSON may wrap a union value: {"string": "x"}.
            let (val, hint) = match v { Value::Object(o) if o.len() == 1 && branches.iter().any(|b| type_name(named(names, b)) == *o.keys().next().unwrap() || b.get("name").and_then(|n| n.as_str()) == o.keys().next().map(|x| x.as_str())) => (o.values().next().unwrap().clone(), o.keys().next().cloned()), _ => (v.clone(), None) };
            let idx = branches.iter().position(|b| match &hint { Some(h) => type_name(named(names, b)) == *h || b.get("name").and_then(|n| n.as_str()) == Some(h.as_str()), None => fits(names, b, &val) })
                .ok_or_else(|| format!("{}: {v} fits none of the union's types", if path.is_empty() { "value" } else { path }))?;
            w.varint(idx as i64);
            return enc(names, &branches[idx], &val, w, path);
        }
        _ => {}
    }
    match (type_name(s).as_str(), v) {
        ("null", Value::Null) => {}
        ("null", _) => return bad("null"),
        ("boolean", Value::Bool(b)) => { w.0.push(*b as u8); }
        ("int" | "long", Value::Number(n)) if n.is_i64() => { w.varint(n.as_i64().unwrap()); }
        ("float", Value::Number(n)) => w.0.extend((n.as_f64().unwrap_or(0.0) as f32).to_le_bytes()),
        ("double", Value::Number(n)) => w.0.extend(n.as_f64().unwrap_or(0.0).to_le_bytes()),
        ("string" | "bytes", Value::String(t)) => { w.varint(t.len() as i64); w.0.extend(t.as_bytes()); }
        ("fixed", Value::String(t)) => w.0.extend(t.as_bytes()),
        ("enum", Value::String(t)) => {
            let i = s["symbols"].as_array().and_then(|a| a.iter().position(|x| x == t)).ok_or_else(|| format!("{path}: “{t}” is not a symbol of {}", s["name"]))?;
            w.varint(i as i64);
        }
        ("array", Value::Array(a)) => {
            if !a.is_empty() {
                w.varint(a.len() as i64);
                for (i, x) in a.iter().enumerate() { enc(names, &s["items"], x, w, &format!("{path}[{i}]"))?; }
            }
            w.varint(0);
        }
        ("map", Value::Object(o)) => {
            if !o.is_empty() {
                w.varint(o.len() as i64);
                for (k, x) in o { w.varint(k.len() as i64); w.0.extend(k.as_bytes()); enc(names, &s["values"], x, w, &format!("{path}.{k}"))?; }
            }
            w.varint(0);
        }
        ("record", Value::Object(o)) => {
            for f in s["fields"].as_array().cloned().unwrap_or_default() {
                let n = f["name"].as_str().unwrap_or("");
                let val = o.get(n).cloned().or_else(|| f.get("default").cloned()).ok_or_else(|| format!("{}{n} is required by the schema", if path.is_empty() { String::new() } else { format!("{path}.") }))?;
                enc(names, &f["type"], &val, w, &if path.is_empty() { n.to_string() } else { format!("{path}.{n}") })?;
            }
        }
        (t, _) => return bad(t),
    }
    Ok(())
}

pub fn decode(schema: &Value, b: &[u8]) -> Result<Value, String> {
    let mut names = HashMap::new();
    collect(schema, &mut names);
    let mut r = R::new(b);
    dec(&names, schema, &mut r)
}

/// Like decode, refusing bytes left after the datum (validating one message against a schema).
pub fn decode_exact(schema: &Value, b: &[u8]) -> Result<Value, String> {
    let mut names = HashMap::new();
    collect(schema, &mut names);
    let mut r = R::new(b);
    let v = dec(&names, schema, &mut r)?;
    if r.left() > 0 { return Err(format!("{} bytes left after the Avro datum", r.left())) }
    Ok(v)
}

fn dec(names: &HashMap<String, Value>, s: &Value, r: &mut R) -> Result<Value, String> {
    let s = named(names, s);
    if let Value::Array(br) = s {
        let i = r.varint()? as usize;
        return dec(names, br.get(i).ok_or("union index out of range")?, r);
    }
    Ok(match type_name(s).as_str() {
        "null" => Value::Null,
        "boolean" => json!(r.i8()? != 0),
        "int" | "long" => json!(r.varint()?),
        "float" => { let mut x = [0u8; 4]; for k in &mut x { *k = r.i8()? as u8 } json!(f32::from_le_bytes(x)) }
        "double" => { let mut x = [0u8; 8]; for k in &mut x { *k = r.i8()? as u8 } json!(f64::from_le_bytes(x)) }
        "string" | "bytes" => json!(String::from_utf8_lossy(r.vbytes()?.unwrap_or(&[]))),
        "fixed" => { let n = s["size"].as_u64().unwrap_or(0) as usize; let mut v = vec![]; for _ in 0..n { v.push(r.i8()? as u8) } json!(String::from_utf8_lossy(&v)) }
        "enum" => { let i = r.varint()? as usize; s["symbols"].get(i).cloned().unwrap_or(Value::Null) }
        "array" => {
            let mut out = vec![];
            loop { let mut n = r.varint()?; if n == 0 { break } if n < 0 { n = -n; r.varint()?; } for _ in 0..n { out.push(dec(names, &s["items"], r)?); } }
            Value::Array(out)
        }
        "map" => {
            let mut out = Map::new();
            loop { let mut n = r.varint()?; if n == 0 { break } if n < 0 { n = -n; r.varint()?; } for _ in 0..n { let k = String::from_utf8_lossy(r.vbytes()?.unwrap_or(&[])).to_string(); out.insert(k, dec(names, &s["values"], r)?); } }
            Value::Object(out)
        }
        "record" => {
            let mut out = Map::new();
            for f in s["fields"].as_array().cloned().unwrap_or_default() { out.insert(f["name"].as_str().unwrap_or("").to_string(), dec(names, &f["type"], r)?); }
            Value::Object(out)
        }
        t => return Err(format!("type {t} is not read")),
    })
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn round_trip() {
        let s = json!({"type": "record", "name": "ShipmentEvent", "namespace": "acme", "fields": [
            {"name": "shipment", "type": "string"}, {"name": "status", "type": {"type": "enum", "name": "Status", "symbols": ["CREATED", "DELIVERED"]}},
            {"name": "weight", "type": ["null", "double"], "default": null}, {"name": "tags", "type": {"type": "array", "items": "string"}},
            {"name": "attrs", "type": {"type": "map", "values": "long"}}, {"name": "carrier", "type": ["null", {"type": "record", "name": "Carrier", "fields": [{"name": "name", "type": "string"}]}]}]});
        let v = json!({"shipment": "SHP-1042", "status": "DELIVERED", "weight": 12.5, "tags": ["a", "b"], "attrs": {"stops": 3}, "carrier": {"name": "Globex"}});
        let b = encode(&s, &v).unwrap();
        assert_eq!(decode(&s, &b).unwrap(), v);
        assert!(encode(&s, &json!({"shipment": "x", "status": "LOST", "tags": [], "attrs": {}, "carrier": null})).unwrap_err().contains("LOST"));
    }
}
