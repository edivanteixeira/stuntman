//! Expressions of plugin files: a CEL-like subset over JSON values. Paths (body.cpf,
//! headers["x-id"]), == != < <= > >= in, && || !, + - * / %, lists, maps, functions and methods.
//! Templates are text with {{ expr | filter(args) }} holes, used by answers and webhooks.
//! ponytail: a small interpreter, not full CEL (no macros like all()/exists() over lists); add
//! them when a plugin needs them.

use serde_json::{json, Map, Value};
use sha2::Digest;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Id(String),
    Op(String),
}

fn lex(s: &str) -> Result<Vec<Tok>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch.is_ascii_digit() {
            let st = i;
            while i < c.len() && (c[i].is_ascii_digit() || (c[i] == '.' && c.get(i + 1).map(|x| x.is_ascii_digit()).unwrap_or(false))) {
                i += 1;
            }
            out.push(Tok::Num(c[st..i].iter().collect::<String>().parse().map_err(|_| "bad number")?));
        } else if ch == '"' || ch == '\'' {
            let q = ch;
            i += 1;
            let mut v = String::new();
            while i < c.len() && c[i] != q {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                    v.push(match c[i] { 'n' => '\n', 't' => '\t', x => x });
                } else {
                    v.push(c[i]);
                }
                i += 1;
            }
            if i >= c.len() {
                return Err("a string is not closed".into());
            }
            i += 1;
            out.push(Tok::Str(v));
        } else if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let st = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '$') {
                i += 1;
            }
            out.push(Tok::Id(c[st..i].iter().collect()));
        } else {
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            if ["==", "!=", "<=", ">=", "&&", "||"].contains(&two.as_str()) {
                out.push(Tok::Op(two));
                i += 2;
            } else if ch == '≠' {
                out.push(Tok::Op("!=".into()));
                i += 1;
            } else if "+-*/%<>!.,()[]{}:?|".contains(ch) {
                out.push(Tok::Op(ch.to_string()));
                i += 1;
            } else {
                return Err(format!("unexpected “{ch}”"));
            }
        }
    }
    Ok(out)
}

#[derive(Clone, Debug)]
enum E {
    Lit(Value),
    Var(String),
    Get(Box<E>, Box<E>),
    Call(String, Vec<E>),
    Method(Box<E>, String, Vec<E>),
    Un(String, Box<E>),
    Bin(String, Box<E>, Box<E>),
    If(Box<E>, Box<E>, Box<E>),
    List(Vec<E>),
    Map(Vec<(String, E)>),
}

struct P {
    t: Vec<Tok>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&Tok> {
        self.t.get(self.i)
    }
    fn is(&self, op: &str) -> bool {
        matches!(self.peek(), Some(Tok::Op(o)) if o == op) || matches!(self.peek(), Some(Tok::Id(o)) if o == op && op == "in")
    }
    fn eat(&mut self, op: &str) -> Result<(), String> {
        if self.is(op) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected “{op}”"))
        }
    }
    fn expr(&mut self) -> Result<E, String> {
        let c = self.or()?;
        if self.is("?") {
            self.i += 1;
            let a = self.expr()?;
            self.eat(":")?;
            let b = self.expr()?;
            return Ok(E::If(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }
    fn or(&mut self) -> Result<E, String> {
        let mut l = self.and()?;
        while self.is("||") {
            self.i += 1;
            l = E::Bin("||".into(), Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<E, String> {
        let mut l = self.cmp()?;
        while self.is("&&") {
            self.i += 1;
            l = E::Bin("&&".into(), Box::new(l), Box::new(self.cmp()?));
        }
        Ok(l)
    }
    fn cmp(&mut self) -> Result<E, String> {
        let l = self.add()?;
        for op in ["==", "!=", "<=", ">=", "<", ">", "in"] {
            if self.is(op) {
                self.i += 1;
                return Ok(E::Bin(op.into(), Box::new(l), Box::new(self.add()?)));
            }
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<E, String> {
        let mut l = self.mul()?;
        while self.is("+") || self.is("-") {
            let op = if self.is("+") { "+" } else { "-" };
            self.i += 1;
            l = E::Bin(op.into(), Box::new(l), Box::new(self.mul()?));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<E, String> {
        let mut l = self.un()?;
        while self.is("*") || self.is("/") || self.is("%") {
            let op = if self.is("*") { "*" } else if self.is("/") { "/" } else { "%" };
            self.i += 1;
            l = E::Bin(op.into(), Box::new(l), Box::new(self.un()?));
        }
        Ok(l)
    }
    fn un(&mut self) -> Result<E, String> {
        if self.is("!") {
            self.i += 1;
            return Ok(E::Un("!".into(), Box::new(self.un()?)));
        }
        if self.is("-") {
            self.i += 1;
            return Ok(E::Un("-".into(), Box::new(self.un()?)));
        }
        self.post()
    }
    fn args(&mut self) -> Result<Vec<E>, String> {
        let mut a = vec![];
        self.eat("(")?;
        while !self.is(")") {
            a.push(self.expr()?);
            if !self.is(")") {
                self.eat(",")?;
            }
        }
        self.eat(")")?;
        Ok(a)
    }
    fn post(&mut self) -> Result<E, String> {
        let mut e = self.prim()?;
        loop {
            if self.is(".") {
                self.i += 1;
                let Some(Tok::Id(n)) = self.peek().cloned() else { return Err("a name after “.”".into()) };
                self.i += 1;
                if self.is("(") {
                    let a = self.args()?;
                    e = E::Method(Box::new(e), n, a);
                } else {
                    e = E::Get(Box::new(e), Box::new(E::Lit(json!(n))));
                }
            } else if self.is("[") {
                self.i += 1;
                let k = self.expr()?;
                self.eat("]")?;
                e = E::Get(Box::new(e), Box::new(k));
            } else {
                return Ok(e);
            }
        }
    }
    fn prim(&mut self) -> Result<E, String> {
        let t = self.peek().cloned().ok_or("the expression ends too early")?;
        self.i += 1;
        Ok(match t {
            Tok::Num(n) => E::Lit(if n.fract() == 0.0 && n.abs() < 9e15 { json!(n as i64) } else { json!(n) }),
            Tok::Str(s) => E::Lit(json!(s)),
            Tok::Id(id) => match id.as_str() {
                "true" => E::Lit(json!(true)),
                "false" => E::Lit(json!(false)),
                "null" => E::Lit(Value::Null),
                _ if self.is("(") => {
                    let a = self.args()?;
                    E::Call(id, a)
                }
                _ => E::Var(id),
            },
            Tok::Op(o) if o == "(" => {
                let e = self.expr()?;
                self.eat(")")?;
                e
            }
            Tok::Op(o) if o == "[" => {
                let mut v = vec![];
                while !self.is("]") {
                    v.push(self.expr()?);
                    if !self.is("]") {
                        self.eat(",")?;
                    }
                }
                self.eat("]")?;
                E::List(v)
            }
            Tok::Op(o) if o == "{" => {
                let mut v = vec![];
                while !self.is("}") {
                    let k = match self.peek().cloned() {
                        Some(Tok::Str(s)) | Some(Tok::Id(s)) => s,
                        _ => return Err("a key in a map".into()),
                    };
                    self.i += 1;
                    self.eat(":")?;
                    v.push((k, self.expr()?));
                    if !self.is("}") {
                        self.eat(",")?;
                    }
                }
                self.eat("}")?;
                E::Map(v)
            }
            Tok::Op(o) => return Err(format!("unexpected “{o}”")),
        })
    }
}

/// Values an expression can read: body, query, headers, path, state, data, and named lets.
pub type Scope = Map<String, Value>;

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The lets in the order they can be computed: one that names another runs after it
/// (YAML maps arrive sorted, so `b: a + 1` before `a: 2` still works). Cycles keep the sorted order.
pub fn let_order(lets: &std::collections::BTreeMap<String, String>) -> Vec<(&String, &String)> {
    let uses = |e: &str, k: &str| lex(e).map(|t| t.iter().any(|t| matches!(t, Tok::Id(x) if x == k))).unwrap_or(false);
    let mut left: Vec<(&String, &String)> = lets.iter().collect();
    let mut out = vec![];
    while !left.is_empty() {
        let i = left.iter().position(|(_, e)| !left.iter().any(|(k, _)| uses(e, k))).unwrap_or(0);
        out.push(left.remove(i));
    }
    out
}

pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0);
            if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { n.to_string() }
        }
        o => o.to_string(),
    }
}

fn eq(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) if a.is_number() || b.is_number() => (x - y).abs() < 1e-9,
        _ => text(a) == text(b) && a.is_null() == b.is_null(),
    }
}

fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// "123.456.789-09" → "123.•••.•••-09" (CPF), else keeps the first 3 and last 2.
pub fn mask(s: &str) -> String {
    let d = digits(s);
    if d.len() == 11 {
        return format!("{}.•••.•••-{}", &d[..3], &d[9..]);
    }
    if s.len() <= 5 {
        return "•••".into();
    }
    format!("{}•••{}", &s[..3.min(s.len())], &s[s.len() - 2..])
}

/// Fixed offsets for the time zones plugins name. ponytail: no DST rules; add chrono-tz when a
/// provider in a DST zone needs exact local time.
fn tz_offset(z: &str) -> Option<i64> {
    Some(match z {
        "UTC" | "Z" | "Etc/UTC" => 0,
        "America/Sao_Paulo" | "America/Bahia" | "America/Fortaleza" | "America/Recife" => -3,
        "America/Manaus" | "America/Cuiaba" => -4,
        "America/Rio_Branco" => -5,
        "America/New_York" => -5,
        "America/Chicago" => -6,
        "America/Los_Angeles" => -8,
        "Europe/Lisbon" | "Europe/London" => 0,
        "Europe/Madrid" | "Europe/Paris" | "Europe/Berlin" => 1,
        _ => return None,
    } * 3_600_000)
}

pub fn iso(ms: i64, offset: Option<i64>) -> String {
    let local = ms + offset.unwrap_or(0);
    let (s, days) = ((local / 1000).rem_euclid(86_400), (local / 1000).div_euclid(86_400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    let base = format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", s / 3600, s % 3600 / 60, s % 60);
    if offset.is_none() { format!("{base}Z") } else { base }
}

/// The CPF check digits are right (synthetic CPFs pass; typos don't).
pub fn cpf_ok(s: &str) -> bool {
    let d: Vec<u32> = digits(s).chars().filter_map(|c| c.to_digit(10)).collect();
    if d.len() != 11 || d.iter().all(|x| *x == d[0]) {
        return false;
    }
    let dv = |n: usize| {
        let s: u32 = (0..n).map(|i| d[i] * (n as u32 + 1 - i as u32)).sum();
        let r = (s * 10) % 11;
        if r == 10 { 0 } else { r }
    };
    dv(9) == d[9] && dv(10) == d[10]
}

fn call(name: &str, a: &[Value], sc: &Scope) -> Result<Value, String> {
    let s0 = || a.first().map(text).unwrap_or_default();
    Ok(match name {
        "size" => json!(match a.first() { Some(Value::Array(x)) => x.len(), Some(Value::Object(x)) => x.len(), Some(v) => text(v).chars().count(), None => 0 }),
        "has" | "exists" => json!(a.first().map(|v| !v.is_null()).unwrap_or(false)),
        "default" => if a.first().map(|v| v.is_null()).unwrap_or(true) { a.get(1).cloned().unwrap_or(Value::Null) } else { a[0].clone() },
        "string" => json!(s0()),
        "int" => json!(num(a.first().unwrap_or(&Value::Null)).map(|x| x as i64)),
        "double" => json!(num(a.first().unwrap_or(&Value::Null))),
        "upper" => json!(s0().to_uppercase()),
        "lower" => json!(s0().to_lowercase()),
        "trim" => json!(s0().trim()),
        // cookie(headers.cookie, 'sid'): one cookie's value from a Cookie header.
        "cookie" => {
            let name = a.get(1).map(text).unwrap_or_default();
            json!(s0().split(';').filter_map(|kv| kv.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string()))
        }
        "replace" => json!(s0().replace(&a.get(1).map(text).unwrap_or_default(), &a.get(2).map(text).unwrap_or_default())),
        "split" => json!(s0().split(&a.get(1).map(text).unwrap_or(",".into())).map(str::to_string).collect::<Vec<_>>()),
        "join" => json!(a.first().and_then(Value::as_array).map(|x| x.iter().map(text).collect::<Vec<_>>().join(&a.get(1).map(text).unwrap_or(",".into()))).unwrap_or_default()),
        // substring(s, start, end?): by characters; a negative start counts from the end.
        "substring" => {
            let c: Vec<char> = s0().chars().collect();
            let n = c.len() as i64;
            let at = |v: Option<&Value>, d: i64| v.and_then(num).map(|x| x as i64).map(|x| if x < 0 { (n + x).max(0) } else { x.min(n) }).unwrap_or(d);
            let (st, en) = (at(a.get(1), 0), at(a.get(2), n));
            json!(c[st as usize..en.max(st) as usize].iter().collect::<String>())
        }
        "digits" => json!(digits(&s0())),
        "mask" => json!(mask(&s0())),
        "cpf_ok" => json!(cpf_ok(&s0())),
        "sha256" => json!(hex::encode(sha2::Sha256::digest(s0().as_bytes()))),
        // base64url(json({"jti": uuid()})): JWT parts, no padding.
        "base64" => json!(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, s0().as_bytes())),
        "base64url" => json!(base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, s0().as_bytes())),
        "uuid" => json!(stuntman_core::util::uuid()),
        // random_id(): 8 letters and digits, for ids that change on every call ("ch_{{ random_id() }}").
        "random_id" => json!(stuntman_core::util::uuid().replace('-', "")[..8].to_string()),
        "now" => json!(iso(clock(sc), None)),
        "now_ms" => json!(clock(sc)),
        // Java-style patterns (look-around) from imported mocks fall back to fancy-regex.
        "matches" => {
            let re = a.get(1).map(text).unwrap_or_default();
            json!(match regex::Regex::new(&re) { Ok(r) => r.is_match(&s0()), Err(_) => fancy_regex::Regex::new(&re).ok().and_then(|r| r.is_match(&s0()).ok()).unwrap_or(false) })
        }
        "json" => json!(serde_json::to_string(a.first().unwrap_or(&Value::Null)).unwrap_or_default()),
        // lookup("people", "cpf", body.cpf): the first row of a data table whose field equals the value.
        "lookup" => {
            let table = sc.get("data").and_then(|d| d.get(&s0())).cloned().unwrap_or(Value::Null);
            let (field, val) = (a.get(1).map(text).unwrap_or_default(), a.get(2).cloned().unwrap_or(Value::Null));
            table.as_array().and_then(|rows| rows.iter().find(|r| r.get(&field).map(|x| eq(x, &val) || digits(&text(x)) == digits(&text(&val)) && !digits(&text(&val)).is_empty()).unwrap_or(false)).cloned()).unwrap_or(Value::Null)
        }
        // cert_pem(person), sign_all(person, body.hashes, 'hex' | 'base64'): see people.rs.
        "cert_pem" => json!(crate::people::cert_pem(a.first().unwrap_or(&Value::Null))?),
        "sign_all" => crate::people::sign_all(a.first().unwrap_or(&Value::Null), a.get(1).unwrap_or(&Value::Null), &a.get(2).map(text).unwrap_or("hex".into()))?,
        "sign_raw" => json!(crate::people::sign_raw(a.first().unwrap_or(&Value::Null), &a.get(1).map(text).unwrap_or_default(), &a.get(2).map(text).unwrap_or("hex".into()))?),
        _ => return Err(format!("no function “{name}”")),
    })
}

fn contains(hay: &Value, needle: &Value) -> bool {
    match hay {
        Value::Array(a) => a.iter().any(|x| eq(x, needle) || x.as_object().map(|o| o.values().any(|v| eq(v, needle) || (!digits(&text(needle)).is_empty() && digits(&text(v)) == digits(&text(needle)) && text(needle).len() >= 11))).unwrap_or(false)),
        Value::Object(o) => o.contains_key(&text(needle)),
        Value::String(s) => s.contains(&text(needle)),
        _ => false,
    }
}

fn ev(e: &E, sc: &Scope) -> Result<Value, String> {
    Ok(match e {
        E::Lit(v) => v.clone(),
        E::Var(n) => sc.get(n).cloned().unwrap_or(Value::Null),
        E::Get(o, k) => {
            let o = ev(o, sc)?;
            let k = ev(k, sc)?;
            match &o {
                Value::Array(a) => num(&k).and_then(|i| a.get(i as usize)).cloned().unwrap_or(Value::Null),
                Value::Object(m) => m.get(&text(&k)).cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            }
        }
        E::Call(n, a) => {
            let a = a.iter().map(|x| ev(x, sc)).collect::<Result<Vec<_>, _>>()?;
            call(n, &a, sc)?
        }
        E::Method(o, n, a) => {
            let o = ev(o, sc)?;
            let a = a.iter().map(|x| ev(x, sc)).collect::<Result<Vec<_>, _>>()?;
            let s = text(&o);
            let a0 = a.first().map(text).unwrap_or_default();
            match n.as_str() {
                "contains" => json!(contains(&o, a.first().unwrap_or(&Value::Null))),
                "startsWith" => json!(s.starts_with(&a0)),
                "endsWith" => json!(s.ends_with(&a0)),
                "matches" => json!(regex::Regex::new(&a0).map(|r| r.is_match(&s)).unwrap_or(false)),
                "size" => call("size", &[o], sc)?,
                "upper" | "lower" | "digits" | "mask" | "sha256" | "base64url" | "cpf_ok" | "string" | "int" => call(n, &[o], sc)?,
                _ => return Err(format!("no method “{n}”")),
            }
        }
        E::Un(op, x) => {
            let v = ev(x, sc)?;
            if op == "!" { json!(!truthy(&v)) } else { json!(-num(&v).unwrap_or(0.0)) }
        }
        E::If(c, a, b) => if truthy(&ev(c, sc)?) { ev(a, sc)? } else { ev(b, sc)? },
        E::Bin(op, l, r) => {
            if op == "&&" {
                return Ok(json!(truthy(&ev(l, sc)?) && truthy(&ev(r, sc)?)));
            }
            if op == "||" {
                let lv = ev(l, sc)?;
                return Ok(if truthy(&lv) { json!(true) } else { json!(truthy(&ev(r, sc)?)) });
            }
            let (a, b) = (ev(l, sc)?, ev(r, sc)?);
            match op.as_str() {
                "==" => json!(eq(&a, &b)),
                "!=" => json!(!eq(&a, &b)),
                "in" => json!(contains(&b, &a)),
                "<" | "<=" | ">" | ">=" => {
                    let o = match (num(&a), num(&b)) {
                        (Some(x), Some(y)) => x.partial_cmp(&y),
                        _ => Some(text(&a).cmp(&text(&b))),
                    };
                    let o = o.unwrap_or(std::cmp::Ordering::Equal);
                    json!(match op.as_str() { "<" => o.is_lt(), "<=" => o.is_le(), ">" => o.is_gt(), _ => o.is_ge() })
                }
                "+" if a.is_string() || b.is_string() => json!(format!("{}{}", text(&a), text(&b))),
                _ => {
                    let (x, y) = (num(&a).unwrap_or(0.0), num(&b).unwrap_or(0.0));
                    let v = match op.as_str() { "+" => x + y, "-" => x - y, "*" => x * y, "/" => if y == 0.0 { 0.0 } else { x / y }, _ => if y == 0.0 { 0.0 } else { x % y } };
                    json!(v)
                }
            }
        }
        E::List(v) => Value::Array(v.iter().map(|x| ev(x, sc)).collect::<Result<_, _>>()?),
        E::Map(v) => Value::Object(v.iter().map(|(k, x)| Ok((k.clone(), ev(x, sc)?))).collect::<Result<_, String>>()?),
    })
}

pub fn parse(s: &str) -> Result<(), String> {
    compile(s).map(|_| ())
}

fn compile(s: &str) -> Result<E, String> {
    let t = lex(s)?;
    let mut p = P { t, i: 0 };
    let e = p.expr()?;
    if p.i < p.t.len() {
        return Err(format!("unexpected “{}” after the expression", match &p.t[p.i] { Tok::Op(o) | Tok::Id(o) | Tok::Str(o) => o.clone(), Tok::Num(n) => n.to_string() }));
    }
    Ok(e)
}

pub fn eval(s: &str, sc: &Scope) -> Result<Value, String> {
    ev(&compile(s)?, sc)
}

pub fn test(s: &str, sc: &Scope) -> Result<bool, String> {
    eval(s, sc).map(|v| truthy(&v))
}

/// Names an expression reads at the top level (for "no field tg on person" checks).
pub fn paths(s: &str) -> Vec<String> {
    let Ok(t) = lex(s) else { return vec![] };
    let mut out = vec![];
    let mut i = 0;
    while i < t.len() {
        if let Tok::Id(n) = &t[i] {
            let prev_dot = i > 0 && t[i - 1] == Tok::Op(".".into());
            let call = t.get(i + 1) == Some(&Tok::Op("(".into()));
            if !prev_dot && !call && !["true", "false", "null", "in"].contains(&n.as_str()) {
                let mut p = n.clone();
                let mut j = i + 1;
                while t.get(j) == Some(&Tok::Op(".".into())) {
                    if let Some(Tok::Id(m)) = t.get(j + 1) {
                        if t.get(j + 2) == Some(&Tok::Op("(".into())) {
                            break;
                        }
                        p.push('.');
                        p.push_str(m);
                        j += 2;
                    } else {
                        break;
                    }
                }
                out.push(p);
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn filter(v: Value, f: &str, sc: &Scope) -> Result<Value, String> {
    let (name, args) = match f.find('(') {
        Some(i) => (f[..i].trim(), f[i..].trim()),
        None => (f.trim(), ""),
    };
    let args: Vec<Value> = if args.is_empty() { vec![] } else {
        match eval(&format!("[{}]", &args[1..args.len().saturating_sub(1)]), sc)? { Value::Array(a) => a, _ => vec![] }
    };
    Ok(match name {
        "default" => if v.is_null() || v == json!("") { args.first().cloned().unwrap_or(Value::Null) } else { v },
        "tz" => {
            let z = args.first().map(text).unwrap_or_default();
            let ms = v.as_i64().unwrap_or_else(stuntman_core::util::now_ms);
            json!(iso(ms, Some(tz_offset(&z).ok_or_else(|| format!("unknown time zone {z}"))?)))
        }
        "json" => json!(serde_json::to_string(&v).unwrap_or_default()),
        "round" => {
            let n = args.first().and_then(num).unwrap_or(0.0) as i32;
            let p = 10f64.powi(n);
            json!((num(&v).unwrap_or(0.0) * p).round() / p)
        }
        other => call(other, &[v], sc)?,
    })
}

/// The environment's time: `__now` in the scope (its clock, A10c), else real time.
pub fn clock(sc: &Scope) -> i64 {
    sc.get("__now").and_then(|v| v.as_i64()).unwrap_or_else(stuntman_core::util::now_ms)
}

/// Renders {{ expr | filter }} holes. Strings go in as they are (write the quotes in the template),
/// numbers and booleans as JSON, lists and maps as JSON.
pub fn render(tpl: &str, sc: &Scope) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let Some(j) = rest[i..].find("}}") else { return Err("a {{ is not closed".into()) };
        let inner = &rest[i + 2..i + j];
        let mut parts = split_pipes(inner).into_iter();
        let head = parts.next().unwrap_or_default();
        let mut v = if head.trim() == "now" { json!(clock(sc)) } else { eval(&head, sc)? };
        for f in parts {
            v = filter(v, &f, sc)?;
        }
        out.push_str(&match &v { Value::String(s) => s.clone(), Value::Null => "null".into(), o => text(o) });
        rest = &rest[i + j + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// "a | b(c || d)" → ["a", "b(c || d)"]: pipes outside parentheses and strings.
fn split_pipes(s: &str) -> Vec<String> {
    let mut out = vec![];
    let (mut depth, mut q, mut cur) = (0, None::<char>, String::new());
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        match (q, ch) {
            (Some(x), _) if ch == x => q = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => q = Some(ch),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, '|') if depth == 0 && c.get(i + 1) != Some(&'|') && (i == 0 || c[i - 1] != '|') => {
                out.push(std::mem::take(&mut cur));
                i += 1;
                continue;
            }
            _ => {}
        }
        cur.push(ch);
        i += 1;
    }
    out.push(cur);
    out
}

/// Every {{ … }} hole of a template parses (for validate_plugin).
pub fn check_template(tpl: &str) -> Result<(), String> {
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        let j = rest[i..].find("}}").ok_or("a {{ is not closed")?;
        let inner = &rest[i + 2..i + j];
        let mut parts = split_pipes(inner).into_iter();
        let head = parts.next().unwrap_or_default();
        if head.trim() != "now" {
            parse(&head)?;
        }
        rest = &rest[i + j + 2..];
    }
    Ok(())
}

#[cfg(test)]
mod t {
    use super::*;

    fn sc() -> Scope {
        let v = json!({"body": {"cpf": "987.654.321-12", "validacao": {"x": 1}}, "data": {"irregular_cpfs": [{"cpf": "98765432112", "situation": "suspended"}],
            "people": [{"cpf": "12345678909", "tag": "ana"}]}, "selfie": {"tag": "bob", "score": 0.92}, "person": {"tag": "ana"}});
        v.as_object().unwrap().clone()
    }

    #[test]
    fn expressions() {
        let s = sc();
        assert!(test("body.cpf in data.irregular_cpfs", &s).unwrap());
        assert!(test("selfie.tag != person.tag", &s).unwrap());
        assert!(test("selfie.tag ≠ person.tag && selfie.score > 0.9", &s).unwrap());
        assert!(!test("has(body.missing)", &s).unwrap());
        assert_eq!(eval("lookup('people', 'cpf', '123.456.789-09').tag", &s).unwrap(), json!("ana"));
        assert_eq!(eval("size(body.cpf) > 10 ? 'long' : 'short'", &s).unwrap(), json!("long"));
        assert!(eval("body.cpf ==", &s).is_err());
        assert!(cpf_ok("123.456.789-09") && !cpf_ok("123.456.789-00"));
        assert_eq!(eval("base64url(json({'alg': 'none'}))", &s).unwrap(), json!("eyJhbGciOiJub25lIn0"));
        assert_eq!(mask("123.456.789-09"), "123.•••.•••-09");
        assert_eq!(eval("split(replace('X-Key:abc', 'X-', ''), ':')[1]", &s).unwrap(), json!("abc"));
        assert_eq!(eval("substring('2026-11-15', 0, 4) + substring('abc', -1)", &s).unwrap(), json!("2026c"));
        assert_eq!(eval("join(split('a b', ' '), '+')", &s).unwrap(), json!("a+b"));
        assert_eq!(eval("{'n': 2}", &s).unwrap(), json!({"n": 2}));
        assert_eq!(eval("base64(':pat')", &s).unwrap(), json!("OnBhdA=="));
        assert_eq!(eval("cookie('a=1; sid=s1', 'sid')", &s).unwrap(), json!("s1"));
        let lets: std::collections::BTreeMap<String, String> = [("a".into(), "b + 1".into()), ("b".into(), "2".into())].into();
        assert_eq!(let_order(&lets).iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), vec!["b", "a"]);
        assert_eq!(paths("selfie.tag != person.tg && size(body.cpf) > 3"), vec!["selfie.tag", "person.tg", "body.cpf"]);
    }

    #[test]
    fn templates() {
        let s = sc();
        assert_eq!(render(r#"{"s": {{ selfie.score | default(0.41) }}, "n": "{{ body.missing | default('x') }}"}"#, &s).unwrap(), r#"{"s": 0.92, "n": "x"}"#);
        assert_eq!(render("{{ body.cpf | mask }}", &s).unwrap(), "987.•••.•••-12");
        assert!(render("{{ now | tz('America/Sao_Paulo') }}", &s).unwrap().len() == 19);
        assert!(check_template("{{ a == }}").is_err());
    }
}
