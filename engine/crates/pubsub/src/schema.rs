//! Schema definitions and message validation: Avro (JSON and binary, through the streams codec) and
//! Protocol Buffers. A .proto definition is parsed here (syntax, package, messages with nested
//! messages and enums, oneofs, maps, repeated and packed fields; imports are refused, as Pub/Sub
//! refuses them) and a message is validated by walking its wire format or its proto3 JSON. The
//! first top-level message is the root. Each validation answers the message as JSON (BigQuery rows).

use std::collections::{HashMap, HashSet};

use base64::Engine;
use serde_json::{json, Map, Value};

pub const PROTOCOL_BUFFER: i32 = 1;
pub const AVRO: i32 = 2;
pub const JSON: i32 = 1;
pub const BINARY: i32 = 2;

/// Checks a definition (CreateSchema, CommitSchema, ValidateSchema).
pub fn check(kind: i32, def: &str) -> Result<(), String> {
    match kind {
        AVRO => {
            let s: Value = serde_json::from_str(def).map_err(|e| format!("Avro schema is not JSON: {e}"))?;
            avro_check(&s, &mut HashSet::new(), "")
        }
        PROTOCOL_BUFFER => Proto::parse(def).map(|_| ()),
        _ => Err("Schema type must be AVRO or PROTOCOL_BUFFER".into()),
    }
}

/// Validates one message; answers what it holds as JSON.
pub fn validate(kind: i32, def: &str, encoding: i32, data: &[u8]) -> Result<Value, String> {
    let json_in = || -> Result<Value, String> {
        let t = std::str::from_utf8(data).map_err(|_| "JSON encoding: the data is not UTF-8".to_string())?;
        serde_json::from_str(t).map_err(|e| format!("JSON encoding: {e}"))
    };
    match (kind, encoding) {
        (AVRO, BINARY) => {
            let s: Value = serde_json::from_str(def).map_err(|e| e.to_string())?;
            stuntman_streams::avro::decode_exact(&s, data)
        }
        (AVRO, _) => {
            let s: Value = serde_json::from_str(def).map_err(|e| e.to_string())?;
            let v = json_in()?;
            stuntman_streams::avro::encode(&s, &v)?;
            Ok(v)
        }
        (PROTOCOL_BUFFER, BINARY) => {
            let p = Proto::parse(def)?;
            p.decode(&p.root, data, 0).map(Value::Object)
        }
        (PROTOCOL_BUFFER, _) => {
            let p = Proto::parse(def)?;
            let v = json_in()?;
            p.check_json(&p.root, &v, "")?;
            Ok(v)
        }
        _ => Err("unknown schema type".into()),
    }
}

// ---------------------------------------------------------------- Avro

const AVRO_PRIMS: [&str; 8] = ["null", "boolean", "int", "long", "float", "double", "bytes", "string"];

fn full_name(o: &Map<String, Value>, ns: &str) -> Result<String, String> {
    let n = o.get("name").and_then(|v| v.as_str()).ok_or("a named type needs a name")?;
    let ns = o.get("namespace").and_then(|v| v.as_str()).unwrap_or(ns);
    Ok(if n.contains('.') || ns.is_empty() { n.to_string() } else { format!("{ns}.{n}") })
}

fn avro_check(s: &Value, names: &mut HashSet<String>, ns: &str) -> Result<(), String> {
    match s {
        Value::String(t) => {
            let known = AVRO_PRIMS.contains(&t.as_str()) || names.contains(t) || names.contains(&format!("{ns}.{t}")) || names.iter().any(|n| n.rsplit('.').next() == Some(t));
            if known { Ok(()) } else { Err(format!("Undefined name: \"{t}\"")) }
        }
        Value::Array(br) => {
            if br.is_empty() { return Err("a union needs at least one type".into()) }
            for b in br {
                if b.is_array() { return Err("Unions may not immediately contain other unions".into()) }
                avro_check(b, names, ns)?;
            }
            Ok(())
        }
        Value::Object(o) => {
            let t = o.get("type").ok_or("a schema object needs a type")?;
            match t.as_str() {
                Some("record") | Some("error") => {
                    let full = full_name(o, ns)?;
                    names.insert(full.clone());
                    let inner = full.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or_default();
                    let fields = o.get("fields").and_then(|v| v.as_array()).ok_or_else(|| format!("record {full} needs fields"))?;
                    let mut seen = HashSet::new();
                    for f in fields {
                        let n = f.get("name").and_then(|v| v.as_str()).ok_or_else(|| format!("a field of {full} has no name"))?;
                        if !seen.insert(n) { return Err(format!("{full}: duplicate field {n}")) }
                        avro_check(f.get("type").ok_or_else(|| format!("{full}.{n} has no type"))?, names, &inner)?;
                    }
                    Ok(())
                }
                Some("enum") => {
                    let full = full_name(o, ns)?;
                    let syms = o.get("symbols").and_then(|v| v.as_array()).ok_or_else(|| format!("enum {full} needs symbols"))?;
                    if syms.iter().any(|x| !x.is_string()) { return Err(format!("enum {full}: symbols are strings")) }
                    names.insert(full);
                    Ok(())
                }
                Some("fixed") => {
                    let full = full_name(o, ns)?;
                    o.get("size").and_then(|v| v.as_u64()).ok_or_else(|| format!("fixed {full} needs a size"))?;
                    names.insert(full);
                    Ok(())
                }
                Some("array") => avro_check(o.get("items").ok_or("an array needs items")?, names, ns),
                Some("map") => avro_check(o.get("values").ok_or("a map needs values")?, names, ns),
                _ => avro_check(t, names, ns),
            }
        }
        _ => Err(format!("Not a schema: {s}")),
    }
}

// ---------------------------------------------------------------- Protocol Buffers

#[derive(Clone, Debug, PartialEq)]
enum Ty {
    Scalar(String),
    Msg(String),
    Enum(String),
}

#[derive(Clone, Debug)]
struct Field {
    name: String,
    json: String,
    num: u64,
    ty: Ty,
    repeated: bool,
    /// map<K, V>: the key and value types.
    map: Option<(Ty, Ty)>,
}

#[derive(Debug, Default)]
pub struct Proto {
    msgs: HashMap<String, Vec<Field>>,
    enums: HashMap<String, Vec<(String, i64)>>,
    root: String,
}

const SCALARS: [&str; 15] = ["double", "float", "int32", "int64", "uint32", "uint64", "sint32", "sint64", "fixed32", "fixed64", "sfixed32", "sfixed64", "bool", "string", "bytes"];

fn tokens(src: &str) -> Result<Vec<String>, String> {
    let c: Vec<char> = src.chars().collect();
    let (mut i, mut out) = (0, vec![]);
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() { i += 1; continue }
        if ch == '/' && c.get(i + 1) == Some(&'/') { while i < c.len() && c[i] != '\n' { i += 1 } continue }
        if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') { i += 1 }
            i += 2;
            continue;
        }
        if ch == '"' || ch == '\'' {
            let mut j = i + 1;
            while j < c.len() && c[j] != ch { if c[j] == '\\' { j += 1 } j += 1 }
            if j >= c.len() { return Err("unterminated string".into()) }
            out.push(c[i..=j].iter().collect());
            i = j + 1;
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '-' || ch == '+' {
            let mut j = i;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '.' || ((c[j] == '-' || c[j] == '+') && j == i)) { j += 1 }
            out.push(c[i..j].iter().collect());
            i = j;
            continue;
        }
        out.push(ch.to_string());
        i += 1;
    }
    Ok(out)
}

fn camel(n: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for ch in n.chars() {
        if ch == '_' { up = true } else if up { out.extend(ch.to_uppercase()); up = false } else { out.push(ch) }
    }
    out
}

struct P {
    t: Vec<String>,
    i: usize,
    /// (scope, field) with the type name still unresolved.
    raw: Vec<(String, String, Field, Option<(String, String)>)>,
    msgs: Vec<String>,
    enums: HashMap<String, Vec<(String, i64)>>,
}

impl P {
    fn peek(&self) -> &str {
        self.t.get(self.i).map(|s| s.as_str()).unwrap_or("")
    }
    fn next(&mut self) -> Result<String, String> {
        let s = self.t.get(self.i).cloned().ok_or("unexpected end of the definition")?;
        self.i += 1;
        Ok(s)
    }
    fn eat(&mut self, want: &str) -> Result<(), String> {
        let got = self.next()?;
        if got == want { Ok(()) } else { Err(format!("expected \"{want}\", found \"{got}\"")) }
    }
    fn skip_stmt(&mut self) -> Result<(), String> {
        let mut depth = 0;
        loop {
            let t = self.next()?;
            match t.as_str() {
                "{" | "[" | "(" => depth += 1,
                "}" | "]" | ")" => depth -= 1,
                ";" if depth == 0 => return Ok(()),
                _ => {}
            }
            if depth == 0 && t == "}" { return Ok(()) }
        }
    }
    fn skip_block(&mut self) -> Result<(), String> {
        while self.peek() != "{" { self.next()?; }
        let mut depth = 0;
        loop {
            match self.next()?.as_str() {
                "{" => depth += 1,
                "}" => { depth -= 1; if depth == 0 { return Ok(()) } }
                _ => {}
            }
        }
    }
    fn options(&mut self) -> Result<(), String> {
        if self.peek() == "[" {
            let mut depth = 0;
            loop {
                match self.next()?.as_str() { "[" => depth += 1, "]" => { depth -= 1; if depth == 0 { break } } _ => {} }
            }
        }
        Ok(())
    }
    fn field(&mut self, scope: &str, ty: String, repeated: bool) -> Result<(), String> {
        let name = self.next()?;
        self.eat("=")?;
        let num: u64 = self.next()?.parse().map_err(|_| format!("{scope}.{name}: field number"))?;
        self.options()?;
        self.eat(";")?;
        let f = Field { json: camel(&name), name, num, ty: Ty::Scalar(String::new()), repeated, map: None };
        self.raw.push((scope.to_string(), ty, f, None));
        Ok(())
    }
    fn enum_(&mut self, scope: &str) -> Result<(), String> {
        let name = self.next()?;
        let full = if scope.is_empty() { name } else { format!("{scope}.{name}") };
        self.eat("{")?;
        let mut vals = vec![];
        loop {
            match self.peek() {
                "}" => { self.next()?; break }
                "option" | "reserved" => self.skip_stmt()?,
                ";" => { self.next()?; }
                _ => {
                    let n = self.next()?;
                    self.eat("=")?;
                    let v: i64 = self.next()?.parse().map_err(|_| format!("{full}.{n}: value"))?;
                    self.options()?;
                    self.eat(";")?;
                    vals.push((n, v));
                }
            }
        }
        self.enums.insert(full, vals);
        Ok(())
    }
    fn message(&mut self, scope: &str) -> Result<(), String> {
        let name = self.next()?;
        let full = if scope.is_empty() { name } else { format!("{scope}.{name}") };
        self.msgs.push(full.clone());
        self.eat("{")?;
        self.body(&full, false)
    }
    fn body(&mut self, full: &str, oneof: bool) -> Result<(), String> {
        loop {
            let t = self.peek().to_string();
            match t.as_str() {
                "}" => { self.next()?; return Ok(()) }
                ";" => { self.next()?; }
                "message" if !oneof => { self.next()?; self.message(full)?; }
                "enum" if !oneof => { self.next()?; self.enum_(full)?; }
                "option" | "reserved" | "extensions" => self.skip_stmt()?,
                "extend" => self.skip_block()?,
                "oneof" => { self.next()?; self.next()?; self.eat("{")?; self.body(full, true)?; }
                "group" => return Err("groups are not supported".into()),
                "map" => {
                    self.next()?;
                    self.eat("<")?;
                    let k = self.next()?;
                    self.eat(",")?;
                    let v = self.next()?;
                    self.eat(">")?;
                    let n = self.raw.len();
                    self.field(full, String::new(), true)?;
                    self.raw[n].3 = Some((k, v));
                }
                "repeated" => { self.next()?; let ty = self.next()?; self.field(full, ty, true)?; }
                "optional" | "required" => { self.next()?; let ty = self.next()?; self.field(full, ty, false)?; }
                "" => return Err(format!("{full}: missing }}")),
                _ => { let ty = self.next()?; self.field(full, ty, false)?; }
            }
        }
    }
}

impl Proto {
    pub fn parse(src: &str) -> Result<Proto, String> {
        let mut p = P { t: tokens(src)?, i: 0, raw: vec![], msgs: vec![], enums: HashMap::new() };
        let mut pkg = String::new();
        let mut root = String::new();
        while p.i < p.t.len() {
            match p.peek() {
                "syntax" | "edition" | "option" => p.skip_stmt()?,
                "package" => { p.next()?; pkg = p.next()?; p.eat(";")?; }
                "import" => return Err("Imports are not supported in Pub/Sub schema definitions".into()),
                "message" => {
                    p.next()?;
                    let n = p.msgs.len();
                    p.message(&pkg)?;
                    if root.is_empty() { root = p.msgs[n].clone() }
                }
                "enum" => { p.next()?; p.enum_(&pkg)?; }
                "service" | "extend" => p.skip_block()?,
                ";" => { p.next()?; }
                t => return Err(format!("unexpected \"{t}\"")),
            }
        }
        if root.is_empty() { return Err("The definition has no message type".into()) }
        let mut out = Proto { root, enums: p.enums, ..Default::default() };
        for m in &p.msgs { out.msgs.insert(m.clone(), vec![]); }
        for (scope, ty, mut f, map) in p.raw {
            if let Some((k, v)) = map {
                f.map = Some((out.resolve(&scope, &k)?, out.resolve(&scope, &v)?));
            } else {
                f.ty = out.resolve(&scope, &ty)?;
            }
            out.msgs.get_mut(&scope).unwrap().push(f);
        }
        Ok(out)
    }

    fn resolve(&self, scope: &str, name: &str) -> Result<Ty, String> {
        if SCALARS.contains(&name) { return Ok(Ty::Scalar(name.into())) }
        let look = |n: &str| if self.msgs.contains_key(n) { Some(Ty::Msg(n.into())) } else if self.enums.contains_key(n) { Some(Ty::Enum(n.into())) } else { None };
        if let Some(abs) = name.strip_prefix('.') { return look(abs).ok_or_else(|| format!("unknown type {name}")) }
        let mut s = scope.to_string();
        loop {
            let cand = if s.is_empty() { name.to_string() } else { format!("{s}.{name}") };
            if let Some(t) = look(&cand) { return Ok(t) }
            if s.is_empty() { return Err(format!("{scope}: unknown type {name}")) }
            s = s.rsplit_once('.').map(|x| x.0.to_string()).unwrap_or_default();
        }
    }

    fn field<'a>(&'a self, msg: &str, num: u64) -> Option<&'a Field> {
        self.msgs.get(msg)?.iter().find(|f| f.num == num)
    }

    /// The wire format of one message, as proto3 JSON (64-bit integers as strings).
    fn decode(&self, msg: &str, b: &[u8], depth: usize) -> Result<Map<String, Value>, String> {
        if depth > 64 { return Err("nested too deep".into()) }
        let mut r = Rd { b, p: 0 };
        let mut out = Map::new();
        while r.p < b.len() {
            let tag = r.varint()?;
            let (num, wt) = (tag >> 3, (tag & 7) as u8);
            if num == 0 { return Err("field number 0".into()) }
            let Some(f) = self.field(msg, num) else { r.skip(wt)?; continue };
            if let Some((k, v)) = &f.map {
                if wt != 2 { return Err(format!("{}: a map entry is length-delimited", f.name)) }
                let entry = r.len_bytes()?;
                let mut er = Rd { b: entry, p: 0 };
                let (mut key, mut val) = (Value::Null, Value::Null);
                while er.p < entry.len() {
                    let t = er.varint()?;
                    match t >> 3 {
                        1 => key = self.scalar(k, (t & 7) as u8, &mut er, &f.name, depth)?,
                        2 => val = self.scalar(v, (t & 7) as u8, &mut er, &f.name, depth)?,
                        _ => er.skip((t & 7) as u8)?,
                    }
                }
                let key = match key { Value::String(s) => s, Value::Null => String::new(), x => x.to_string() };
                out.entry(f.json.clone()).or_insert_with(|| json!({})).as_object_mut().unwrap().insert(key, val);
                continue;
            }
            let packable = matches!(&f.ty, Ty::Enum(_)) || matches!(&f.ty, Ty::Scalar(s) if !["string", "bytes"].contains(&s.as_str()));
            if f.repeated && packable && wt == 2 {
                let seg = r.len_bytes()?;
                let mut pr = Rd { b: seg, p: 0 };
                let want = wire_type(&f.ty);
                let arr = out.entry(f.json.clone()).or_insert_with(|| json!([])).as_array_mut().unwrap();
                while pr.p < seg.len() { arr.push(self.scalar(&f.ty, want, &mut pr, &f.name, depth)?); }
                continue;
            }
            let v = self.scalar(&f.ty, wt, &mut r, &f.name, depth)?;
            if f.repeated {
                out.entry(f.json.clone()).or_insert_with(|| json!([])).as_array_mut().unwrap().push(v);
            } else {
                out.insert(f.json.clone(), v);
            }
        }
        Ok(out)
    }

    fn scalar(&self, ty: &Ty, wt: u8, r: &mut Rd, name: &str, depth: usize) -> Result<Value, String> {
        let want = wire_type(ty);
        if wt != want { return Err(format!("{name}: wire type {wt} does not match its type (wants {want})")) }
        Ok(match ty {
            Ty::Msg(m) => Value::Object(self.decode(m, r.len_bytes()?, depth + 1)?),
            Ty::Enum(e) => {
                let n = r.varint()? as i64 as i32 as i64;
                self.enums[e].iter().find(|x| x.1 == n).map(|x| json!(x.0)).unwrap_or(json!(n))
            }
            Ty::Scalar(s) => match s.as_str() {
                "int32" => json!(r.varint()? as i64 as i32),
                "int64" => json!((r.varint()? as i64).to_string()),
                "uint32" => json!(r.varint()? as u32),
                "uint64" => json!(r.varint()?.to_string()),
                "sint32" => { let u = r.varint()?; json!(((u >> 1) as i64 ^ -((u & 1) as i64)) as i32) }
                "sint64" => { let u = r.varint()?; json!(((u >> 1) as i64 ^ -((u & 1) as i64)).to_string()) }
                "bool" => json!(r.varint()? != 0),
                "fixed64" => json!(u64::from_le_bytes(r.take(8)?.try_into().unwrap()).to_string()),
                "sfixed64" => json!(i64::from_le_bytes(r.take(8)?.try_into().unwrap()).to_string()),
                "double" => json!(f64::from_le_bytes(r.take(8)?.try_into().unwrap())),
                "fixed32" => json!(u32::from_le_bytes(r.take(4)?.try_into().unwrap())),
                "sfixed32" => json!(i32::from_le_bytes(r.take(4)?.try_into().unwrap())),
                "float" => json!(f32::from_le_bytes(r.take(4)?.try_into().unwrap())),
                "string" => json!(std::str::from_utf8(r.len_bytes()?).map_err(|_| format!("{name}: a string field holds invalid UTF-8"))?),
                _ => json!(base64::engine::general_purpose::STANDARD.encode(r.len_bytes()?)),
            },
        })
    }

    /// proto3 JSON: every key a field (JSON or proto name), every value of its type.
    fn check_json(&self, msg: &str, v: &Value, path: &str) -> Result<(), String> {
        let o = v.as_object().ok_or_else(|| format!("{}: expected an object", if path.is_empty() { msg } else { path }))?;
        let fields = &self.msgs[msg];
        for (k, x) in o {
            let f = fields.iter().find(|f| &f.json == k || &f.name == k).ok_or_else(|| format!("{msg} has no field \"{k}\""))?;
            let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            if x.is_null() { continue }
            if let Some((_, vt)) = &f.map {
                let m = x.as_object().ok_or_else(|| format!("{p}: a map is a JSON object"))?;
                for (mk, mv) in m { self.json_value(vt, mv, &format!("{p}.{mk}"))?; }
            } else if f.repeated {
                let a = x.as_array().ok_or_else(|| format!("{p}: a repeated field is a JSON array"))?;
                for (i, e) in a.iter().enumerate() { self.json_value(&f.ty, e, &format!("{p}[{i}]"))?; }
            } else {
                self.json_value(&f.ty, x, &p)?;
            }
        }
        Ok(())
    }

    fn json_value(&self, ty: &Ty, v: &Value, p: &str) -> Result<(), String> {
        let bad = |want: &str| Err(format!("{p}: expected {want}, got {v}"));
        let int_ok = |v: &Value, lo: i128, hi: i128| -> bool {
            let n: Option<i128> = match v { Value::Number(n) => n.as_i64().map(|x| x as i128).or_else(|| n.as_u64().map(|x| x as i128)).or_else(|| n.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i128)), Value::String(s) => s.parse().ok(), _ => None };
            n.is_some_and(|n| n >= lo && n <= hi)
        };
        match ty {
            Ty::Msg(m) => self.check_json(m, v, p),
            Ty::Enum(e) => match v {
                Value::String(s) if self.enums[e].iter().any(|x| &x.0 == s) => Ok(()),
                Value::Number(n) if n.is_i64() => Ok(()),
                _ => bad(&format!("a value of {e}")),
            },
            Ty::Scalar(s) => match s.as_str() {
                "int32" | "sint32" | "sfixed32" => if int_ok(v, i32::MIN as i128, i32::MAX as i128) { Ok(()) } else { bad("a 32-bit integer") },
                "uint32" | "fixed32" => if int_ok(v, 0, u32::MAX as i128) { Ok(()) } else { bad("an unsigned 32-bit integer") },
                "int64" | "sint64" | "sfixed64" => if int_ok(v, i64::MIN as i128, i64::MAX as i128) { Ok(()) } else { bad("a 64-bit integer") },
                "uint64" | "fixed64" => if int_ok(v, 0, u64::MAX as i128) { Ok(()) } else { bad("an unsigned 64-bit integer") },
                "float" | "double" => match v { Value::Number(_) => Ok(()), Value::String(s) if ["NaN", "Infinity", "-Infinity"].contains(&s.as_str()) || s.parse::<f64>().is_ok() => Ok(()), _ => bad("a number") },
                "bool" => if v.is_boolean() { Ok(()) } else { bad("true or false") },
                "string" => if v.is_string() { Ok(()) } else { bad("a string") },
                _ => match v.as_str() {
                    Some(s) if base64::engine::general_purpose::STANDARD.decode(s).is_ok() || base64::engine::general_purpose::URL_SAFE.decode(s).is_ok() => Ok(()),
                    _ => bad("base64 bytes"),
                },
            },
        }
    }
}

fn wire_type(ty: &Ty) -> u8 {
    match ty {
        Ty::Msg(_) => 2,
        Ty::Enum(_) => 0,
        Ty::Scalar(s) => match s.as_str() {
            "fixed64" | "sfixed64" | "double" => 1,
            "fixed32" | "sfixed32" | "float" => 5,
            "string" | "bytes" => 2,
            _ => 0,
        },
    }
}

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.p + n > self.b.len() { return Err("truncated message".into()) }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn varint(&mut self) -> Result<u64, String> {
        let (mut u, mut shift) = (0u64, 0);
        loop {
            let b = self.take(1)?[0];
            u |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 { return Ok(u) }
            shift += 7;
            if shift > 63 { return Err("malformed varint".into()) }
        }
    }
    fn len_bytes(&mut self) -> Result<&'a [u8], String> {
        let n = self.varint()? as usize;
        self.take(n)
    }
    fn skip(&mut self, wt: u8) -> Result<(), String> {
        match wt {
            0 => { self.varint()?; }
            1 => { self.take(8)?; }
            2 => { self.len_bytes()?; }
            5 => { self.take(4)?; }
            _ => return Err(format!("wire type {wt} is not valid")),
        }
        Ok(())
    }
}

#[cfg(test)]
mod t {
    use super::*;

    const PROTO: &str = r#"
        syntax = "proto3";
        package acme.logistics;
        // A shipment as the carrier reports it.
        message ShipmentEvent {
          string shipment_id = 1;
          Status status = 2;
          double weight_kg = 3;
          repeated string tags = 4;
          map<string, int64> stops = 5;
          Carrier carrier = 6;
          oneof ref { string order_id = 7; int32 batch = 8; }
          enum Status { STATUS_UNSPECIFIED = 0; CREATED = 1; DELIVERED = 2; }
          message Carrier { string name = 1; }
          repeated int32 codes = 9 [packed = true];
        }"#;

    fn pb(fields: &[(u64, u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![];
        for (n, wt, v) in fields {
            out.push(((n << 3) | *wt as u64) as u8);
            if *wt == 2 { out.push(v.len() as u8) }
            out.extend(v);
        }
        out
    }

    #[test]
    fn proto_binary_and_json() {
        check(PROTOCOL_BUFFER, PROTO).unwrap();
        let carrier = pb(&[(1, 2, b"Globex".to_vec())]);
        let stop = pb(&[(1, 2, b"Springfield".to_vec()), (2, 0, vec![3])]);
        let msg = pb(&[(1, 2, b"SHP-1042".to_vec()), (2, 0, vec![2]), (3, 1, 12.5f64.to_le_bytes().to_vec()), (4, 2, b"fragile".to_vec()), (5, 2, stop), (6, 2, carrier), (9, 2, vec![1, 2, 3])]);
        let v = validate(PROTOCOL_BUFFER, PROTO, BINARY, &msg).unwrap();
        assert_eq!(v, json!({"shipmentId": "SHP-1042", "status": "DELIVERED", "weightKg": 12.5, "tags": ["fragile"], "stops": {"Springfield": "3"}, "carrier": {"name": "Globex"}, "codes": [1, 2, 3]}));
        // Wrong wire type for shipment_id; truncated bytes.
        assert!(validate(PROTOCOL_BUFFER, PROTO, BINARY, &pb(&[(1, 0, vec![5])])).unwrap_err().contains("shipment_id"));
        assert!(validate(PROTOCOL_BUFFER, PROTO, BINARY, &[0x0a, 0x09, b'S']).is_err());
        // JSON: camel or proto names, unknown fields and wrong types refused.
        validate(PROTOCOL_BUFFER, PROTO, JSON, br#"{"shipment_id": "SHP-1", "status": "CREATED", "stops": {"a": "2"}, "carrier": {"name": "x"}, "batch": 3}"#).unwrap();
        assert!(validate(PROTOCOL_BUFFER, PROTO, JSON, br#"{"shipmentId": 7}"#).unwrap_err().contains("expected a string"));
        assert!(validate(PROTOCOL_BUFFER, PROTO, JSON, br#"{"color": "red"}"#).unwrap_err().contains("no field"));
        assert!(validate(PROTOCOL_BUFFER, PROTO, JSON, br#"{"status": "LOST"}"#).is_err());
        assert!(check(PROTOCOL_BUFFER, "import \"x.proto\"; message A {}").is_err());
        assert!(check(PROTOCOL_BUFFER, "message A { Missing m = 1; }").unwrap_err().contains("unknown type"));
    }

    #[test]
    fn avro_definitions_and_messages() {
        let s = r#"{"type": "record", "name": "ShipmentEvent", "fields": [{"name": "shipment", "type": "string"}, {"name": "weight", "type": ["null", "double"], "default": null}]}"#;
        check(AVRO, s).unwrap();
        assert!(check(AVRO, r#"{"type": "record", "name": "A", "fields": [{"name": "x", "type": "Nope"}]}"#).unwrap_err().contains("Undefined"));
        assert!(check(AVRO, "not json").is_err());
        validate(AVRO, s, JSON, br#"{"shipment": "SHP-1", "weight": 2.5}"#).unwrap();
        assert!(validate(AVRO, s, JSON, br#"{"weight": 2.5}"#).is_err());
        let b = stuntman_streams::avro::encode(&serde_json::from_str(s).unwrap(), &json!({"shipment": "SHP-2", "weight": null})).unwrap();
        assert_eq!(validate(AVRO, s, BINARY, &b).unwrap(), json!({"shipment": "SHP-2", "weight": null}));
        let mut extra = b.clone();
        extra.push(0);
        assert!(validate(AVRO, s, BINARY, &extra).is_err());
    }
}
