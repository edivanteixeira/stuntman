//! WireMock response templates (A11): answers with `template: handlebars` render like WireMock's
//! response-template transformer, so imported mocks keep working. The request is `request` (url,
//! path, pathSegments, query, headers, body, method, baseUrl); the helpers are WireMock's: now,
//! randomValue, randomInt, math, numberFormat, xPath, jsonPath, regexExtract, parseJson, assign,
//! size, toJson, trim, replace, contains and the comparisons.

use handlebars::{Context, Handlebars, Helper, HelperDef, HelperResult, JsonRender, Output, RenderContext, RenderError, RenderErrorReason, Renderable, ScopedJson, StringOutput};
use serde_json::{json, Map, Value};

type F = fn(&[Value], &Map<String, Value>) -> Result<Value, String>;

struct V(F);

impl HelperDef for V {
    fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, _: &'rc Context, _: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
        let args: Vec<Value> = h.params().iter().map(|p| p.value().clone()).collect();
        let hash: Map<String, Value> = h.hash().iter().map(|(k, v)| (k.to_string(), v.value().clone())).collect();
        (self.0)(&args, &hash).map(ScopedJson::Derived).map_err(|e| RenderErrorReason::Other(format!("{}: {e}", h.name())).into())
    }

    /// Block form, as WireMock: {{#eq a b}}yes{{else}}no{{/eq}}.
    fn call<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, r: &'reg Handlebars<'reg>, ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>, out: &mut dyn Output) -> HelperResult {
        let v = self.call_inner(h, r, ctx, rc)?.as_json().clone();
        if h.is_block() {
            // WireMock's {{#matches}} renders nothing (not the else) when it does not match.
            if h.name() == "matches" && !truthy(&v) {
                return Ok(());
            }
            return match if truthy(&v) { h.template() } else { h.inverse() } { Some(t) => t.render(r, ctx, rc, out), None => Ok(()) };
        }
        // jsonPath prints an object or a list as JSON (Jackson's pretty printer), as in WireMock.
        out.write(&if h.name() == "jsonPath" && (v.is_object() || v.is_array()) { jackson(&v, 0) } else { v.render() })?;
        Ok(())
    }
}

/// Jackson's default pretty printer: {\n  "a" : 1,\n  "b" : [ 1, 2 ]\n}.
fn jackson(v: &Value, level: usize) -> String {
    match v {
        Value::Object(o) if o.is_empty() => "{ }".into(),
        Value::Object(o) => {
            let pad = "  ".repeat(level + 1);
            let items: Vec<String> = o.iter().map(|(k, x)| format!("{pad}{} : {}", Value::String(k.clone()), jackson(x, level + 1))).collect();
            format!("{{\n{}\n{}}}", items.join(",\n"), "  ".repeat(level))
        }
        Value::Array(a) if a.is_empty() => "[ ]".into(),
        Value::Array(a) => format!("[ {} ]", a.iter().map(|x| jackson(x, level + 1)).collect::<Vec<_>>().join(", ")),
        // parseJson keeps a decimal's text (-26.30) as a string; it is still a number here.
        Value::String(t) if t.parse::<f64>().is_ok() && t.contains('.') && t.ends_with('0') => t.clone(),
        x => x.to_string(),
    }
}

/// parseJson reads decimals as Java's BigDecimal does: -26.30 stays -26.30 and 2.50e1 is 25.0. A number
/// whose text a double would change is kept as a string (math, comparisons and printing read it as is).
fn keep_decimals(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let (mut i, mut in_str) = (0, false);
    while i < c.len() {
        let ch = c[i];
        if in_str {
            out.push(ch);
            if ch == '\\' && i + 1 < c.len() {
                out.push(c[i + 1]);
                i += 1;
            } else if ch == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_str = true;
        }
        if ch == '-' || ch.is_ascii_digit() {
            let mut j = i + 1;
            while j < c.len() && (c[j].is_ascii_digit() || ".eE+-".contains(c[j])) {
                j += 1;
            }
            let tok: String = c[i..j].iter().collect();
            if tok.contains(['.', 'e', 'E']) {
                let (m, sc) = dec(&json!(tok));
                let digits = m.abs().to_string();
                let text = if sc == 0 { m.to_string() } else {
                    let d = format!("{:0>w$}", digits, w = sc as usize + 1);
                    let (a, b) = d.split_at(d.len() - sc as usize);
                    format!("{}{a}.{b}", if m < 0 { "-" } else { "" })
                };
                let as_double = tok.parse::<f64>().map(|f| serde_json::Number::from_f64(f).map(|n| n.to_string()).unwrap_or_default()).unwrap_or_default();
                if text != as_double {
                    out.push_str(&format!("\"{text}\""));
                    i = j;
                    continue;
                }
            }
            out.push_str(&tok);
            i = j;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

/// A variable the rest of the template reads ({{name.x}}), as WireMock's assign, parseJson, formData.
fn set_var<'reg: 'rc, 'rc>(ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>, name: String, val: Value) -> HelperResult {
    let mut data = rc.context().map(|c| c.data().clone()).unwrap_or_else(|| ctx.data().clone());
    if let Some(o) = data.as_object_mut() {
        o.insert(name, val);
    }
    rc.set_context(Context::wraps(data)?);
    Ok(())
}

/// A bare {{x}}: a request parameter or header is a list, and prints its first value, as WireMock's.
struct Out;

impl HelperDef for Out {
    fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, _: &'rc Context, _: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
        let v = h.param(0).map(|p| p.value().clone()).unwrap_or(Value::Null);
        Ok(ScopedJson::Derived(match v { Value::Array(a) => a.into_iter().next().unwrap_or(Value::Null), v => v }))
    }
}

/// A template variable (assign, parseJson, regexExtract, formData) read anywhere, as in WireMock: the
/// current block's own field of that name first, then the variable.
struct Var;

impl HelperDef for Var {
    fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
        let name = h.param(0).map(|p| s(p.value())).unwrap_or_default();
        let segs: Vec<String> = name.split('.').map(|x| x.trim_matches(['[', ']']).to_string()).filter(|x| !x.is_empty()).collect();
        let get = |mut v: &Value, segs: &[String]| -> Option<Value> {
            for k in segs {
                v = match v { Value::Object(o) => o.get(k)?, Value::Array(a) => a.get(k.parse::<usize>().ok()?)?, _ => return None };
            }
            Some(v.clone()).filter(|v| !v.is_null())
        };
        let cur = rc.context();
        let root = cur.as_ref().map(|c| c.data()).unwrap_or(ctx.data());
        let local = rc.block().and_then(|b| match b.base_value() {
            Some(v) => get(v, &segs),
            None if !b.base_path().is_empty() => get(root, b.base_path()).and_then(|v| get(&v, &segs)),
            None => None,
        });
        Ok(local.or_else(|| get(root, &segs)).map(ScopedJson::Derived).unwrap_or(ScopedJson::Missing))
    }
}

/// The variables a template makes: {{#assign "x"}}, {{#parseJson "x"}}, {{parseJson v 'x'}},
/// {{regexExtract v 're' 'x'}}, {{formData v 'x'}}.
fn variables(t: &str) -> Vec<String> {
    let tag = regex::Regex::new(r"(?s)\{\{\{?~?\s*#?\s*(assign|parseJson|regexExtract|formData)\b(.*?)\}\}").unwrap();
    let hash = regex::Regex::new(r#"\w+=("[^"]*"|'[^']*'|\S+)"#).unwrap();
    let quoted = regex::Regex::new(r#""([^"]*)"|'([^']*)'"#).unwrap();
    let ident = regex::Regex::new(r"^[A-Za-z_]\w*$").unwrap();
    let mut out = vec![];
    for c in tag.captures_iter(t) {
        let rest = hash.replace_all(&c[2], "");
        let q: Vec<String> = quoted.captures_iter(&rest).map(|m| m.get(1).or(m.get(2)).map(|x| x.as_str().to_string()).unwrap_or_default()).collect();
        let min = if &c[1] == "regexExtract" { 2 } else { 1 };
        if let Some(v) = q.last().filter(|v| q.len() >= min && ident.is_match(v)) {
            if !out.contains(v) {
                out.push(v.clone());
            }
        }
    }
    out
}

/// Reads of those variables become (__var "x.y"), so they resolve inside each/with/if as in WireMock.
fn global_vars(t: &str) -> String {
    let vars = variables(t);
    if vars.is_empty() {
        return t.to_string();
    }
    let tag = regex::Regex::new(r"(?s)\{\{(\{?)(~?)(.*?)(~?)(\}?)\}\}").unwrap();
    let tok = regex::Regex::new(r#"(^|[\s(=])((?:\.\./)*)([A-Za-z_]\w*)((?:\.(?:\[[^\]]*\]|[\w-]+))*)"#).unwrap();
    tag.replace_all(t, |c: &regex::Captures| {
        let inner = &c[3];
        if inner.starts_with('!') || inner.trim_start().starts_with('/') {
            return c[0].to_string();
        }
        // The first word names the helper when the tag is a block or has parameters ({{#each x}}, {{math x '+' 1}}).
        let body0 = inner.trim_start_matches(['#', '^', '&', ' ']);
        let head_len = if inner.trim_start().starts_with(['#', '^']) || body0.trim().contains(char::is_whitespace) { inner.len() - body0.len() + body0.find(char::is_whitespace).unwrap_or(body0.len()) } else { 0 };
        let (head, params) = inner.split_at(head_len);
        // Outside quoted strings only.
        let mut out = head.to_string();
        let mut rest = params;
        while !rest.is_empty() {
            let q = rest.find(['"', '\'']).unwrap_or(rest.len());
            let (code, tail) = rest.split_at(q);
            out.push_str(&tok.replace_all(code, |m: &regex::Captures| {
                if !vars.iter().any(|v| v == &m[3]) {
                    return m[0].to_string();
                }
                format!("{}(__var \"{}{}\")", &m[1], &m[3], &m[4])
            }));
            if tail.is_empty() {
                break;
            }
            let qc = tail.chars().next().unwrap();
            let end = tail[1..].find(qc).map(|i| i + 2).unwrap_or(tail.len());
            out.push_str(&tail[..end]);
            rest = &tail[end..];
        }
        let trimmed = out.trim();
        let body = if trimmed.starts_with("(__var ") && trimmed.ends_with(')') && trimmed.matches("(__var").count() == 1 && !trimmed[1..].contains('(') { format!("__out {trimmed}") } else { out };
        format!("{{{{{}{}{}{}{}}}}}", &c[1], &c[2], body, &c[4], &c[5])
    })
    .into_owned()
}

/// {{formData request.body 'f'}} (or the block form): the urlencoded body as variable f, each field a
/// list ({{f.k}}, {{f.k.[1]}}); urlDecode=true decodes the values.
struct FormData;

impl HelperDef for FormData {
    fn call<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>, out: &mut dyn Output) -> HelperResult {
        let decode = h.hash_get("urlDecode").map(|v| truthy(v.value()) && v.value() != &json!("false")).unwrap_or(false);
        let mut m = Map::new();
        for kv in s(&h.param(0).map(|p| p.value().clone()).unwrap_or(Value::Null)).split('&').filter(|x| !x.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let d = |x: &str| if decode { crate::engine::netserve_decode(&x.replace('+', " ")) } else { x.to_string() };
            if let Value::Array(a) = m.entry(d(k)).or_insert(json!([])) {
                a.push(json!(d(v)));
            }
        }
        match h.param(1).map(|p| s(p.value())) {
            Some(name) => set_var(ctx, rc, name, Value::Object(m)),
            None => {
                out.write(&Value::Object(m).render())?;
                Ok(())
            }
        }
    }
}

/// {{#assign "name"}}…{{/assign}} and {{#parseJson "name"}}…{{/parseJson}}: the rendered block
/// becomes a variable of the template (a string, or parsed JSON).
struct Assign(bool);

impl HelperDef for Assign {
    fn call<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, r: &'reg Handlebars<'reg>, ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>, out: &mut dyn Output) -> HelperResult {
        // parseJson can also be inline: {{parseJson request.body 'var'}} or {{#parseJson request.body}}.
        let (name, src) = match (h.param(0).map(|p| p.value().clone()), h.param(1).map(|p| p.value().clone())) {
            (Some(a), Some(Value::String(n))) => (n, Some(a)),
            (Some(Value::String(n)), None) if h.template().is_some() => (n, None),
            (Some(a), None) if self.0 => (String::new(), Some(a)),
            _ => return Err(RenderErrorReason::Other(format!("{}: a variable name", h.name())).into()),
        };
        let text = match src {
            Some(Value::String(s)) => s,
            Some(v) => v.to_string(),
            None => {
                let mut buf = StringOutput::new();
                if let Some(t) = h.template() {
                    t.render(r, ctx, rc, &mut buf)?;
                }
                buf.into_string().unwrap_or_default().replace(LINE, "")
            }
        };
        let val = if self.0 {
            // Not JSON: a template error (WireMock answers 500), not an empty variable.
            serde_json::from_str(&keep_decimals(text.trim())).map_err(|e| RenderErrorReason::Other(format!("parseJson: {e}")))?
        } else {
            Value::String(text)
        };
        if name.is_empty() {
            if let Some(t) = h.template() {
                let old = rc.context().map(|c| (*c).clone()).unwrap_or_else(|| ctx.clone());
                rc.set_context(Context::wraps(val)?);
                let res = t.render(r, ctx, rc, out);
                rc.set_context(old);
                return res;
            }
            return Ok(());
        }
        set_var(ctx, rc, name, val)
    }
}

/// {{regexExtract value 'regex'}} gives the match; with a third parameter it keeps the groups as
/// that variable ({{rl.0}}), or `default` when nothing matches.
struct RegexExtract;

impl HelperDef for RegexExtract {
    fn call<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, ctx: &'rc Context, rc: &mut RenderContext<'reg, 'rc>, out: &mut dyn Output) -> HelperResult {
        let v = s(&h.param(0).map(|p| p.value().clone()).unwrap_or(Value::Null));
        let re = fancy_regex::Regex::new(&s(&h.param(1).map(|p| p.value().clone()).unwrap_or(Value::Null))).map_err(|e| RenderErrorReason::Other(format!("regexExtract: {e}")))?;
        let caps = re.captures(&v).ok().flatten();
        match h.param(2).map(|p| s(p.value())) {
            Some(var) => {
                let val = match caps {
                    Some(c) if c.len() > 1 => Value::Array((1..c.len()).map(|i| json!(c.get(i).map(|m| m.as_str()).unwrap_or(""))).collect()),
                    Some(c) => json!([c.get(0).map(|m| m.as_str()).unwrap_or("")]),
                    None => match h.hash_get("default").map(|d| d.value().clone()) { Some(Value::String(d)) => serde_json::from_str(&d).unwrap_or(json!([d])), Some(d) => d, None => json!([]) },
                };
                set_var(ctx, rc, var, val)
            }
            None => {
                let m = caps.and_then(|c| c.get(0).map(|m| m.as_str().to_string())).unwrap_or_default();
                out.write(&m)?;
                Ok(())
            }
        }
    }
}

/// Handlebars.java takes "\[" in a string; handlebars-rust only knows \" and \\ there.
fn java_strings(t: &str) -> String {
    let c: Vec<char> = t.chars().collect();
    let mut out = String::with_capacity(t.len());
    let (mut in_tag, mut q, mut i) = (false, None::<char>, 0);
    while i < c.len() {
        let ch = c[i];
        if !in_tag && ch == '{' && c.get(i + 1) == Some(&'{') {
            in_tag = true;
        } else if in_tag && q.is_none() && ch == '}' && c.get(i + 1) == Some(&'}') {
            in_tag = false;
        } else if in_tag && q.is_none() && (ch == '"' || ch == '\'') {
            q = Some(ch);
        } else if in_tag && Some(ch) == q {
            q = None;
        } else if in_tag && q.is_some() && ch == '\\' {
            let next = c.get(i + 1).copied().unwrap_or(' ');
            if next == q.unwrap() || next == '\\' {
                out.push(ch);
                out.push(next);
                i += 2;
                continue;
            }
            out.push_str("\\\\");
            i += 1;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        // A parameter or header list reads as its first value (WireMock's ListOrSingle).
        Value::Array(a) => a.first().map(s).unwrap_or_default(),
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

fn n(v: &Value) -> f64 {
    match v {
        Value::Number(x) => x.as_f64().unwrap_or(0.0),
        Value::String(x) => x.trim().replace(',', ".").parse().unwrap_or(0.0),
        Value::Bool(b) => *b as i64 as f64,
        Value::Array(a) => a.first().map(n).unwrap_or(0.0),
        _ => 0.0,
    }
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).cloned().unwrap_or(Value::Null)
}

/// "-5 minutes", "3 days", "1 hours" → ms.
fn offset_ms(o: &str) -> i64 {
    let mut it = o.split_whitespace();
    let k: f64 = it.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let unit = it.next().unwrap_or("seconds").to_lowercase();
    let m = match unit.trim_end_matches('s') {
        "millisecond" => 1.0,
        "second" => 1000.0,
        "minute" => 60_000.0,
        "hour" => 3_600_000.0,
        "day" => 86_400_000.0,
        "week" => 604_800_000.0,
        "month" => 2_592_000_000.0,
        "year" => 31_536_000_000.0,
        _ => 1000.0,
    };
    (k * m) as i64
}

/// Java SimpleDateFormat (yyyy MM dd HH mm ss SSS, 'literals', Z/X zones) for one instant.
pub fn java_format(ms: i64, fmt: &str, tz_min: i32) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).unwrap_or(time::OffsetDateTime::UNIX_EPOCH).to_offset(time::UtcOffset::from_whole_seconds(tz_min * 60).unwrap_or(time::UtcOffset::UTC));
    if fmt.is_empty() {
        return format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}", t.year(), t.month() as u8, t.day(), t.hour(), t.minute(), t.second(), if tz_min == 0 { "Z".to_string() } else { zone(tz_min, true) });
    }
    if fmt == "epoch" {
        return ms.to_string();
    }
    if fmt == "unix" {
        return (ms / 1000).to_string();
    }
    let c: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        if c[i] == '\'' {
            let mut j = i + 1;
            while j < c.len() && c[j] != '\'' {
                out.push(c[j]);
                j += 1;
            }
            if j == i + 1 {
                out.push('\'');
            }
            i = j + 1;
            continue;
        }
        let mut j = i;
        while j < c.len() && c[j] == c[i] {
            j += 1;
        }
        let k = j - i;
        match c[i] {
            'y' => out.push_str(&if k == 2 { format!("{:02}", t.year() % 100) } else { format!("{:04}", t.year()) }),
            'M' => out.push_str(&format!("{:0w$}", t.month() as u8, w = k.min(2))),
            'd' => out.push_str(&format!("{:0w$}", t.day(), w = k.min(2))),
            'H' => out.push_str(&format!("{:0w$}", t.hour(), w = k.min(2))),
            'm' => out.push_str(&format!("{:0w$}", t.minute(), w = k.min(2))),
            's' => out.push_str(&format!("{:0w$}", t.second(), w = k.min(2))),
            'S' => out.push_str(&format!("{:03}", t.millisecond())[..k.min(3)]),
            'Z' => out.push_str(&zone(tz_min, false)),
            'X' => out.push_str(&if tz_min == 0 { "Z".into() } else { zone(tz_min, k >= 3) }),
            'x' => out.push_str(&zone(tz_min, k >= 3)),
            ch => (0..k).for_each(|_| out.push(ch)),
        }
        i = j;
    }
    out
}

fn zone(min: i32, colon: bool) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    let a = min.abs();
    if colon { format!("{sign}{:02}:{:02}", a / 60, a % 60) } else { format!("{sign}{:02}{:02}", a / 60, a % 60) }
}

fn tz_minutes(tz: &str) -> i32 {
    match tz {
        "" | "UTC" | "GMT" | "Z" => 0,
        "America/Sao_Paulo" | "America/Fortaleza" | "America/Recife" | "America/Bahia" | "Brazil/East" | "BRT" => -180,
        "America/Manaus" | "America/Cuiaba" | "America/Campo_Grande" | "America/Porto_Velho" => -240,
        "America/Rio_Branco" => -300,
        "America/New_York" => -300,
        "America/Chicago" => -360,
        "America/Los_Angeles" => -480,
        "Europe/London" => 0,
        "Europe/Lisbon" => 0,
        "Europe/Madrid" | "Europe/Paris" | "Europe/Berlin" => 60,
        x => x.trim_start_matches("GMT").trim_start_matches("UTC").split(':').next().and_then(|h| h.parse::<i32>().ok()).map(|h| h * 60).unwrap_or(0),
    }
}

fn now_h(_: &[Value], h: &Map<String, Value>) -> Result<Value, String> {
    let now = h.get("__now").map(n).map(|x| x as i64).unwrap_or_else(stuntman_core::util::now_ms);
    let ms = now + h.get("offset").map(|o| offset_ms(&s(o))).unwrap_or(0);
    Ok(json!(java_format(ms, &h.get("format").map(s).unwrap_or_default(), tz_minutes(&h.get("timezone").map(s).unwrap_or_default()))))
}

fn random_value(_: &[Value], h: &Map<String, Value>) -> Result<Value, String> {
    let len = h.get("length").map(n).unwrap_or(36.0) as usize;
    let ty = h.get("type").map(s).unwrap_or("ALPHANUMERIC".into()).to_uppercase();
    if ty == "UUID" {
        return Ok(json!(stuntman_core::util::uuid()));
    }
    // Lower case unless uppercase=true, as WireMock.
    let set: &[u8] = match ty.as_str() {
        "NUMERIC" => b"0123456789",
        "ALPHABETIC" => b"abcdefghijklmnopqrstuvwxyz",
        "HEXADECIMAL" => b"0123456789abcdef",
        "ALPHANUMERIC_AND_SYMBOLS" => b"abcdefghijklmnopqrstuvwxyz0123456789!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~",
        _ => b"abcdefghijklmnopqrstuvwxyz0123456789",
    };
    let mut v: String = (0..len).map(|_| set[rand::random::<usize>() % set.len()] as char).collect();
    if h.get("uppercase").map(|x| x == &json!(true) || x == "true").unwrap_or(false) {
        v = v.to_uppercase();
    }
    Ok(json!(v))
}

fn random_int(_: &[Value], h: &Map<String, Value>) -> Result<Value, String> {
    let lo = h.get("lower").map(n).unwrap_or(i32::MIN as f64) as i64;
    let hi = h.get("upper").map(n).unwrap_or(i32::MAX as f64) as i64;
    Ok(json!(if hi > lo { lo + (rand::random::<u64>() % (hi - lo) as u64) as i64 } else { lo }))
}

/// A decimal as Java's BigDecimal holds it: mantissa and scale ("-23.5614" = -235614, 4).
fn dec(v: &Value) -> (i128, u32) {
    let t = match v { Value::Array(a) => a.first().map(s).unwrap_or_default(), v => s(v) };
    let t = t.trim();
    // Java's double text: 1.23456785E7, 1.0E-4.
    let (t, e) = match t.split_once(['E', 'e']) { Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)), None => (t, 0) };
    if t.is_empty() {
        return (0, 0);
    }
    let (i, f) = t.split_once('.').unwrap_or((t, ""));
    let Ok(m) = format!("{i}{f}").parse::<i128>() else { return (n(v).round() as i128, 0) };
    let sc = f.len() as i32 - e;
    if sc >= 0 { (m, sc as u32) } else { (m * p10((-sc) as u32), 0) }
}

/// Java's Double.toString: 3.0, -23.5627, 1.0E-4, 1.23456785E7.
fn java_double(f: f64) -> String {
    let a = f.abs();
    if a == 0.0 || (1e-3..1e7).contains(&a) {
        let t = format!("{f}");
        return if t.contains('.') { t } else { format!("{t}.0") };
    }
    let t = format!("{f:e}");
    let (m, e) = t.split_once('e').unwrap_or((&t, "0"));
    format!("{}E{e}", if m.contains('.') { m.to_string() } else { format!("{m}.0") })
}

fn p10(k: u32) -> i128 {
    10i128.pow(k)
}

/// WireMock's math: BigDecimal arithmetic (0.1 + 0.2 = 0.3), division at the left operand's scale
/// rounded half up (10 / 4 = 3); a whole result prints as an integer, the others as a Java double.
fn math(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let ((xm, xs), op, (ym, ys)) = (dec(&arg(a, 0)), s(&arg(a, 1)), dec(&arg(a, 2)));
    let sc = xs.max(ys);
    let (xa, ya) = (xm * p10(sc - xs), ym * p10(sc - ys));
    let (m, scale) = match op.as_str() {
        "+" => (xa + ya, sc),
        "-" => (xa - ya, sc),
        "*" => (xm * ym, xs + ys),
        "/" | "%" if ym == 0 => return Err("/ by zero".into()),
        "/" => {
            let (num, den) = (xm * p10(ys), ym);
            let (q, r) = (num / den, num % den);
            (if 2 * r.abs() >= den.abs() { q + num.signum() * den.signum() } else { q }, xs)
        }
        "%" => (xa % ya, sc),
        o => return Err(format!("operator {o}")),
    };
    if scale == 0 {
        return Ok(json!(m as i64));
    }
    Ok(json!(java_double(m as f64 / p10(scale) as f64)))
}

/// numberFormat n '0.00' | '00000' | '#,##0.00' | 'integer' | 'currency'
fn number_format(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let x = n(&arg(a, 0));
    let f = s(&arg(a, 1));
    if f == "integer" {
        return Ok(json!(format!("{}", x.round() as i64)));
    }
    let (int_part, dec_part) = f.split_once('.').unwrap_or((&f, ""));
    let decimals = dec_part.chars().filter(|c| *c == '0' || *c == '#').count();
    let min_int = int_part.chars().filter(|c| *c == '0').count();
    let grouping = int_part.contains(',');
    let t = format!("{:.*}", decimals, x.abs());
    let (i, d) = t.split_once('.').unwrap_or((&t, ""));
    let mut i = format!("{:0>w$}", i, w = min_int.max(1));
    if grouping {
        let b: Vec<char> = i.chars().collect();
        i = b.iter().enumerate().fold(String::new(), |mut acc, (k, ch)| {
            if k > 0 && (b.len() - k) % 3 == 0 {
                acc.push(',');
            }
            acc.push(*ch);
            acc
        });
    }
    let d = d.trim_end_matches(|c| c == '0' && dec_part.contains('#') && !dec_part.contains('0'));
    Ok(json!(format!("{}{}{}{}", if x < 0.0 { "-" } else { "" }, i, if d.is_empty() { "" } else { "." }, d)))
}

/// A location path over the body, for imported matchesXPath: `/` and `//` steps, `*` or a name (its
/// prefix ignored), predicates joined by `and` (local-name()='x', name()='x', text()='v', .='v',
/// child='v', @a, @a='v', contains(text()|.|@a, 'v'), starts-with(…), a position [1]), and a last
/// step text() or @a. The string values found; None when the expression is outside this subset.
pub fn xpath_select(xml: &str, expr: &str) -> Option<Vec<String>> {
    let steps = xpath_steps(expr.trim())?;
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Some(vec![]) };
    let mut cur: Vec<roxmltree::Node> = vec![doc.root()];
    let mut out: Option<Vec<String>> = None;
    for (deep, step) in &steps {
        let cands: Vec<roxmltree::Node> = cur.iter().flat_map(|n| if *deep { n.descendants().skip(1).collect::<Vec<_>>() } else { n.children().collect() }).collect();
        match step.as_str() {
            "text()" => out = Some(cands.iter().filter(|n| n.is_text()).map(|n| n.text().unwrap_or("").to_string()).collect()),
            a if a.starts_with('@') => out = Some(cur.iter().filter_map(|n| n.attribute(&a[1..])).map(String::from).collect()),
            _ => {
                let (test, preds) = split_step(step)?;
                let mut next = vec![];
                for n in cands.iter().filter(|n| n.is_element() && (test == "*" || n.tag_name().name() == test.rsplit(':').next().unwrap_or(&test))) {
                    next.push(*n);
                }
                for p in preds {
                    if let Ok(k) = p.trim().parse::<usize>() {
                        next = next.get(k.wrapping_sub(1)).copied().into_iter().collect();
                        continue;
                    }
                    let conds: Vec<&str> = p.split(" and ").collect();
                    let mut keep = vec![];
                    for n in next {
                        let mut ok = true;
                        for c in &conds {
                            ok &= xpath_cond(&n, c.trim())?;
                        }
                        if ok {
                            keep.push(n);
                        }
                    }
                    next = keep;
                }
                cur = next;
                // Dedup (// can reach a node twice).
                cur.dedup_by_key(|n| n.id());
            }
        }
    }
    Some(out.unwrap_or_else(|| cur.iter().map(|n| n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect()).collect()))
}

/// The expression is in the subset xpath_select reads.
pub fn xpath_supported(e: &str) -> bool {
    let Ok(doc) = roxmltree::Document::parse("<x/>") else { return false };
    let x = doc.root_element();
    xpath_steps(e.trim()).map(|steps| steps.iter().all(|(_, st)| st == "text()" || st.starts_with('@') || split_step(st).map(|(_, ps)| ps.iter().all(|p| p.trim().parse::<usize>().is_ok() || p.split(" and ").all(|c| xpath_cond(&x, c.trim()).is_some()))).unwrap_or(false))).unwrap_or(false)
}

fn xpath_steps(e: &str) -> Option<Vec<(bool, String)>> {
    if !e.starts_with('/') {
        return None;
    }
    let c: Vec<char> = e.chars().collect();
    let (mut steps, mut i) = (vec![], 0);
    while i < c.len() {
        let deep = c.get(i + 1) == Some(&'/');
        i += if deep { 2 } else { 1 };
        let (mut j, mut depth, mut q) = (i, 0, None::<char>);
        while j < c.len() && !(c[j] == '/' && depth == 0 && q.is_none()) {
            match c[j] {
                '\'' | '"' if q == Some(c[j]) => q = None,
                '\'' | '"' if q.is_none() => q = Some(c[j]),
                '[' if q.is_none() => depth += 1,
                ']' if q.is_none() => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        let step: String = c[i..j].iter().collect();
        if step.is_empty() {
            return None;
        }
        steps.push((deep, step));
        i = j;
    }
    Some(steps)
}

/// "*[local-name()='a' and text()='b'][1]" → ("*", ["local-name()='a' and text()='b'", "1"]).
fn split_step(step: &str) -> Option<(String, Vec<String>)> {
    let (test, rest) = step.split_once('[').map(|(t, r)| (t, format!("[{r}"))).unwrap_or((step, String::new()));
    if !(test == "*" || test.chars().all(|ch| ch.is_alphanumeric() || "_-.:".contains(ch))) || test.is_empty() {
        return None;
    }
    let mut preds = vec![];
    let (mut depth, mut cur, mut q) = (0, String::new(), None::<char>);
    for ch in rest.chars() {
        match ch {
            '\'' | '"' if q == Some(ch) => q = None,
            '\'' | '"' if q.is_none() => q = Some(ch),
            '[' if q.is_none() => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            ']' if q.is_none() => {
                depth -= 1;
                if depth == 0 {
                    preds.push(std::mem::take(&mut cur));
                    continue;
                }
            }
            _ => {}
        }
        cur.push(ch);
    }
    Some((test.to_string(), preds))
}

fn xpath_cond(n: &roxmltree::Node, c: &str) -> Option<bool> {
    let lit = |x: &str| -> Option<String> {
        let x = x.trim();
        let q = x.chars().next()?;
        (x.len() >= 2 && (q == '\'' || q == '"') && x.ends_with(q)).then(|| x[1..x.len() - 1].to_string())
    };
    let string_value = |n: &roxmltree::Node| n.descendants().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("")).collect::<String>();
    let value_of = |what: &str| -> Option<Vec<String>> {
        Some(match what.trim() {
            "." => vec![string_value(n)],
            "text()" => n.children().filter(|d| d.is_text()).map(|d| d.text().unwrap_or("").to_string()).collect(),
            "local-name()" => vec![n.tag_name().name().to_string()],
            "name()" => vec![n.tag_name().name().to_string()],
            a if a.starts_with('@') => n.attribute(&a[1..]).map(String::from).into_iter().collect(),
            child if child.chars().all(|ch| ch.is_alphanumeric() || "_-.:".contains(ch)) => n.children().filter(|d| d.is_element() && d.tag_name().name() == child.rsplit(':').next().unwrap_or(child)).map(|d| string_value(&d)).collect(),
            _ => return None,
        })
    };
    for (f, op) in [("contains(", 0), ("starts-with(", 1)] {
        if let Some(args) = c.strip_prefix(f).and_then(|r| r.strip_suffix(')')) {
            let (a, b) = args.split_once(',')?;
            let want = lit(b)?;
            return Some(value_of(a)?.iter().any(|v| if op == 0 { v.contains(&want) } else { v.starts_with(&want) }));
        }
    }
    if let Some((a, b)) = c.split_once('=') {
        let want = lit(b)?;
        return Some(value_of(a)?.iter().any(|v| *v == want));
    }
    if c.starts_with('@') {
        return Some(n.attribute(&c[1..]).is_some());
    }
    None
}

/// //*[local-name()='x']/text(), //x/text(), //x/@attr, count(//x): the forms mocks use.
pub fn xpath_eval(xml: &str, expr: &str) -> Value {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Value::Null };
    let e = expr.trim();
    let (count, e) = match e.strip_prefix("count(").and_then(|x| x.strip_suffix(')')) { Some(x) => (true, x), None => (false, e) };
    static LN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"local-name\(\)\s*=\s*['"]([\w.-]+)['"]"#).unwrap());
    let ln = &*LN;
    let mut steps: Vec<String> = vec![];
    for part in e.split('/').filter(|p| !p.is_empty()) {
        steps.push(match ln.captures(part) { Some(c) => c[1].to_string(), None => part.split('[').next().unwrap_or(part).split(':').last().unwrap_or(part).to_string() });
    }
    let (attr, text) = match steps.last().map(|x| x.as_str()) {
        Some("text()") => (None, { steps.pop(); true }),
        Some(a) if a.starts_with('@') => { let a = a[1..].to_string(); steps.pop(); (Some(a), false) }
        _ => (None, true),
    };
    let Some(target) = steps.last().cloned() else { return Value::Null };
    let parent = steps.len().checked_sub(2).map(|i| steps[i].clone());
    let found: Vec<roxmltree::Node> = doc
        .descendants()
        .filter(|n| n.is_element() && (target == "*" || n.tag_name().name() == target))
        .filter(|n| parent.as_ref().map(|p| p == "*" || n.parent_element().map(|x| x.tag_name().name() == p).unwrap_or(false)).unwrap_or(true))
        .collect();
    if count {
        return json!(found.len());
    }
    let one = |n: &roxmltree::Node| -> Value {
        match &attr {
            Some(a) => json!(n.attribute(a.as_str()).unwrap_or("")),
            None if text => json!(n.text().unwrap_or("").trim()),
            None => json!(n.text().unwrap_or("")),
        }
    };
    match found.len() {
        0 => Value::Null,
        1 => one(&found[0]),
        _ => Value::Array(found.iter().map(one).collect()),
    }
}

fn xpath_h(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(xpath_eval(&s(&arg(a, 0)), &s(&arg(a, 1))))
}

pub fn json_path_eval(v: &Value, path: &str) -> Value {
    let p = path.trim().trim_start_matches('$');
    let mut cur = v.clone();
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"\.?([\w-]+)|\[(\d+)\]|\['([^']+)'\]").unwrap());
    for c in RE.captures_iter(p) {
        cur = if let Some(k) = c.get(1).or(c.get(3)) {
            match &cur { Value::String(t) => serde_json::from_str::<Value>(t).ok().and_then(|x| x.get(k.as_str()).cloned()).unwrap_or(Value::Null), x => x.get(k.as_str()).cloned().unwrap_or(Value::Null) }
        } else if let Some(i) = c.get(2) {
            cur.get(i.as_str().parse::<usize>().unwrap_or(0)).cloned().unwrap_or(Value::Null)
        } else {
            cur
        };
    }
    cur
}

fn json_path_h(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let v = match arg(a, 0) { Value::String(t) => serde_json::from_str(&t).unwrap_or(Value::Null), x => x };
    Ok(json_path_eval(&v, &s(&arg(a, 1))))
}

fn size(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(match arg(a, 0) { Value::Array(x) => x.len(), Value::Object(x) => x.len(), Value::String(x) => x.chars().count(), _ => 0 }))
}

fn to_json(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(jackson(&arg(a, 0), 0)))
}

fn trim(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(s(&arg(a, 0)).trim()))
}

fn replace(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(s(&arg(a, 0)).replace(&s(&arg(a, 1)), &s(&arg(a, 2)))))
}

fn contains(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(match arg(a, 0) { Value::Array(x) => x.contains(&arg(a, 1)), v => s(&v).contains(&s(&arg(a, 1))) }))
}

fn cmp(a: &[Value], name: &str) -> bool {
    let (x, y) = (arg(a, 0), arg(a, 1));
    // WireMock compares a list as a list: {{eq request.query.c '1'}} is false, request.query.c.[0] is the value.
    if x.is_array() || x.is_object() || y.is_array() || y.is_object() {
        return match name { "eq" => x == y, "ne" | "neq" => x != y, _ => false };
    }
    let both_num = |v: &Value| v.is_number() || s(v).trim().parse::<f64>().is_ok();
    if both_num(&x) && both_num(&y) {
        let (x, y) = (n(&x), n(&y));
        return match name { "eq" => x == y, "ne" | "neq" => x != y, "lt" => x < y, "gt" => x > y, "lte" => x <= y, _ => x >= y };
    }
    let (x, y) = (s(&x), s(&y));
    match name { "eq" => x == y, "ne" | "neq" => x != y, "lt" => x < y, "gt" => x > y, "lte" => x <= y, _ => x >= y }
}

fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false)) && v != &json!("") && v != &json!(0) && v.as_array().map(|a| !a.is_empty()).unwrap_or(true)
}

macro_rules! cmp_h {
    ($f:ident, $n:literal) => {
        fn $f(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
            Ok(json!(cmp(a, $n)))
        }
    };
}
cmp_h!(eq, "eq");
cmp_h!(ne, "ne");
cmp_h!(lt, "lt");
cmp_h!(gt, "gt");
cmp_h!(lte, "lte");
cmp_h!(gte, "gte");

fn and(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(a.iter().all(truthy)))
}
fn or(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(a.iter().any(truthy)))
}
fn not(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(!truthy(&arg(a, 0))))
}
/// {{substring 'abcdef' 1 3}} → bc; past the end is an error, as in WireMock.
fn substring(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let c: Vec<char> = s(&arg(a, 0)).chars().collect();
    let from = n(&arg(a, 1)) as usize;
    let to = if a.len() > 2 { n(&arg(a, 2)) as usize } else { c.len() };
    if from > to || to > c.len() {
        return Err(format!("begin {from}, end {to}, length {}", c.len()));
    }
    Ok(json!(c[from..to].iter().collect::<String>()))
}

/// {{matches value 'regex'}}: the whole value matches (Java's String.matches).
fn matches(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let re = fancy_regex::Regex::new(&format!(r"\A(?:{})\z", s(&arg(a, 1)))).map_err(|e| e.to_string())?;
    Ok(json!(re.is_match(&s(&arg(a, 0))).unwrap_or(false)))
}

/// {{#each (range 1 3)}} → 1, 2, 3.
fn range(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let (lo, hi) = (n(&arg(a, 0)) as i64, n(&arg(a, 1)) as i64);
    if hi < lo {
        return Err(format!("{}", hi - lo));
    }
    Ok(json!((lo..=hi).collect::<Vec<_>>()))
}

fn upper(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(s(&arg(a, 0)).to_uppercase()))
}
fn lower(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    Ok(json!(s(&arg(a, 0)).to_lowercase()))
}
fn base64_h(a: &[Value], h: &Map<String, Value>) -> Result<Value, String> {
    use base64::Engine;
    let b = base64::engine::general_purpose::STANDARD;
    if h.get("decode").map(|x| x == &json!(true)).unwrap_or(false) {
        return Ok(json!(String::from_utf8_lossy(&b.decode(s(&arg(a, 0))).unwrap_or_default())));
    }
    Ok(json!(b.encode(s(&arg(a, 0)))))
}

fn registry() -> Handlebars<'static> {
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    // WireMock does not HTML-escape {{ }} output (a JSON body keeps its quotes).
    hb.register_escape_fn(handlebars::no_escape);
    let list: [(&str, F); 28] = [("substring", substring), ("matches", matches), ("range", range), ("randomValue", random_value), ("randomInt", random_int), ("math", math), ("numberFormat", number_format), ("xPath", xpath_h),
        ("jsonPath", json_path_h), ("size", size), ("toJson", to_json), ("trim", trim), ("replace", replace), ("contains", contains),
        ("eq", eq), ("ne", ne), ("neq", ne), ("lt", lt), ("gt", gt), ("lte", lte), ("gte", gte), ("and", and), ("or", or), ("not", not), ("upper", upper), ("lower", lower),
        ("base64", base64_h), ("now", now_h)];
    for (k, f) in list {
        hb.register_helper(k, Box::new(V(f)));
    }
    // now() reads the environment's clock, which render() puts in the context.
    struct Now;
    impl HelperDef for Now {
        fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, ctx: &'rc Context, _: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
            let mut hash: Map<String, Value> = h.hash().iter().map(|(k, v)| (k.to_string(), v.value().clone())).collect();
            hash.insert("__now".into(), ctx.data().get("__now").cloned().unwrap_or_else(|| json!(stuntman_core::util::now_ms())));
            now_h(&[], &hash).map(ScopedJson::Derived).map_err(|e| RenderErrorReason::Other(e).into())
        }
    }
    hb.register_helper("now", Box::new(Now));
    hb.register_helper("regexExtract", Box::new(RegexExtract));
    hb.register_helper("assign", Box::new(Assign(false)));
    hb.register_helper("parseJson", Box::new(Assign(true)));
    hb.register_helper("formData", Box::new(FormData));
    hb.register_helper("__out", Box::new(Out));
    hb.register_helper("__var", Box::new(Var));
    hb
}

/// Helpers a bare {{name}} calls (it is not a value to print).
const NO_PARAMS: &[&str] = &["now", "randomValue", "randomInt", "else", "this", "log", "raw", "lookup", "if", "unless", "each", "with"];

/// {{request.query.c}} → {{__out request.query.c}}: a bare value prints its first item when it is a
/// list (WireMock's request parameters and headers); helpers and block tags are left as they are.
fn bare_values(t: &str) -> String {
    let re = regex::Regex::new(r"\{\{(\{?)(~?)\s*([A-Za-z_@.][\w@.\[\]/-]*)\s*(~?)\}\}(\}?)").unwrap();
    re.replace_all(t, |c: &regex::Captures| {
        let p = &c[3];
        if NO_PARAMS.contains(&p) || c[1].is_empty() != c[5].is_empty() {
            return c[0].to_string();
        }
        format!("{{{{{}__out {}{}}}}}", &c[2], p, &c[4])
    })
    .into_owned()
}

/// The `request` WireMock templates read, from Stuntman's scope; header names the template uses
/// (request.headers.SOAPAction) are found whatever their case.
pub fn context(tpl: &str, sc: &Map<String, Value>) -> Value {
    let path = sc.get("__path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
    // Every parameter and header is a list, as in WireMock: request.query.l.[1], {{#each request.query.l}}.
    let raw_q = sc.get("__query").and_then(|v| v.as_str()).map(String::from);
    let mut query = Map::new();
    match &raw_q {
        Some(q) => {
            for kv in q.split('&').filter(|x| !x.is_empty()) {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                let d = |x: &str| crate::engine::netserve_decode(&x.replace('+', " "));
                if let Value::Array(a) = query.entry(d(k)).or_insert(json!([])) {
                    a.push(json!(d(v)));
                }
            }
        }
        None => query = sc.get("query").and_then(|v| v.as_object()).cloned().unwrap_or_default().into_iter().map(|(k, v)| (k, if v.is_array() { v } else { json!([v]) })).collect(),
    }
    let mut headers: Map<String, Value> = sc.get("headers").and_then(|v| v.as_object()).cloned().unwrap_or_default().into_iter().map(|(k, v)| (k, json!([v]))).collect();
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)request\.headers\.\[?([\w-]+)").unwrap());
    for c in RE.captures_iter(tpl) {
        let want = c[1].to_string();
        if let Some(v) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(&want)).map(|x| x.1.clone()) {
            headers.insert(want, v);
        }
    }
    let qs = raw_q.unwrap_or_else(|| query.iter().map(|(k, v)| format!("{k}={}", s(v))).collect::<Vec<_>>().join("&"));
    // As WireMock: host and port from the Host header (the scheme's default port when it has none);
    // baseUrl leaves a default port out.
    let host_header = headers.get("host").map(s).unwrap_or_default();
    let scheme = sc.get("__scheme").and_then(|v| v.as_str()).unwrap_or("http").to_string();
    let default_port: u16 = if scheme == "https" { 443 } else { 80 };
    let (host, port) = match host_header.rsplit_once(':').filter(|(h, p)| !h.ends_with(']') || p.chars().all(|c| c.is_ascii_digit())).and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h.to_string(), p))) {
        Some((h, p)) => (h, p),
        None => (host_header.clone(), default_port),
    };
    let base_url = if port == default_port { format!("{scheme}://{host}") } else { format!("{scheme}://{host}:{port}") };
    let request = json!({
        "url": if qs.is_empty() { path.clone() } else { format!("{path}?{qs}") },
        "path": path, "pathSegments": path.trim_matches('/').split('/').collect::<Vec<_>>(),
        "query": query, "headers": headers, "body": sc.get("raw").cloned().unwrap_or(json!("")),
        "method": sc.get("__method").cloned().unwrap_or(json!("GET")), "baseUrl": base_url, "host": host, "port": port, "scheme": scheme,
    });
    // Webhooks (postServeActions) read the call that triggered them as originalRequest, and the
    // answer it got as originalResponse.
    json!({"request": request, "originalRequest": request, "originalResponse": sc.get("__response").cloned().unwrap_or(Value::Null)})
}

/// Marks the start of each line so Handlebars-rust does not drop "standalone" lines: WireMock's
/// Handlebars.java keeps them. A whitespace character, so ~ still trims across it; removed after rendering.
const LINE: char = '\u{2029}';

fn keep_lines(t: &str) -> String {
    let mut out = String::with_capacity(t.len() + 64);
    out.push(LINE);
    let mut rest = t;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i].replace('\n', "\n\u{2029}"));
        let tag = &rest[i..];
        let end = if tag.starts_with("{{!--") { tag.find("--}}").map(|e| e + 4) } else { tag.find("}}").map(|e| e + 2 + tag[e + 2..].starts_with('}') as usize) }.unwrap_or(tag.len());
        out.push_str(&tag[..end]);
        rest = &tag[end..];
    }
    out.push_str(&rest.replace('\n', "\n\u{2029}"));
    out
}

static REGISTRY: std::sync::LazyLock<std::sync::RwLock<Handlebars<'static>>> = std::sync::LazyLock::new(|| std::sync::RwLock::new(registry()));
static COMPILED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, Result<String, String>>>> = std::sync::LazyLock::new(Default::default);

/// The name a template is registered under, compiled the first time it is seen: every call
/// compiled it again (and built the helpers again), milliseconds per answer.
fn compiled(src: &str) -> Result<String, String> {
    let mut seen = COMPILED.lock().unwrap();
    if let Some(r) = seen.get(src) {
        return r.clone();
    }
    // ponytail: templates live as long as the process; drop them all past 5,000 (edited plugins leave old ones).
    if seen.len() >= 5000 {
        seen.clear();
        *REGISTRY.write().unwrap() = registry();
    }
    let name = format!("t{}", seen.len());
    let r = REGISTRY.write().unwrap().register_template_string(&name, keep_lines(&bare_values(&global_vars(&java_strings(src))))).map(|_| name).map_err(|e| e.to_string());
    seen.insert(src.to_string(), r.clone());
    r
}

pub fn render(tpl: &str, sc: &Map<String, Value>) -> Result<String, String> {
    let now = sc.get("__now").and_then(|v| v.as_i64()).unwrap_or_else(stuntman_core::util::now_ms);
    // WireMock renders only from the first {{ to the last }}: the text around is kept as it is (~ does not trim it).
    let (Some(a), Some(b)) = (tpl.find("{{"), tpl.rfind("}}")) else { return Ok(tpl.to_string()) };
    let b = b + 2 + tpl[b + 2..].starts_with('}') as usize;
    let name = compiled(&tpl[a..b])?;
    let mut data = context(tpl, sc);
    data["__now"] = json!(now);
    let body = REGISTRY.read().unwrap().render(&name, &data).map_err(|e| e.to_string())?;
    Ok(format!("{}{}{}", &tpl[..a], body.replace(LINE, ""), &tpl[b..]))
}

/// The template parses (validate_plugin).
pub fn check(tpl: &str) -> Result<(), String> {
    handlebars::Template::compile(&java_strings(tpl)).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod t {
    use super::*;

    fn sc(body: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("raw".into(), json!(body));
        m.insert("__path".into(), json!("/rastreio/v2/pos"));
        m.insert("query".into(), json!({"externalId": "41"}));
        m.insert("headers".into(), json!({"soapaction": "\"urn:x#Averba\"", "host": "localhost:8028"}));
        m.insert("__now".into(), json!(1_763_190_000_000i64));
        m
    }

    #[test]
    fn wiremock_helpers() {
        let body = r#"<e:Env xmlns:e="x"><e:Body><cUF>35</cUF><item>1</item><item>2</item></e:Body></e:Env>"#;
        let t = r#"{{#assign "uf"}}{{xPath request.body "//*[local-name()='cUF']/text()"}}{{/assign}}uf={{uf}} n={{math request.query.externalId '+' 1}} f={{numberFormat 7 '00000'}} a={{regexExtract request.headers.SOAPAction 'Averba'}} d={{now format="yyyy-MM-dd'T'HH:mm:ss" timezone='America/Sao_Paulo'}} {{#each (xPath request.body "//item/text()")}}[{{@index}}:{{this}}]{{/each}}{{#if (lt 1 2)}}lt{{/if}} r={{size (randomValue length=8 type='NUMERIC')}}"#;
        let out = render(t, &sc(body)).unwrap();
        assert_eq!(out, "uf=35 n=42 f=00007 a=Averba d=2025-11-15T04:00:00 [0:1][1:2]lt r=8", "{out}");
        let j = render(r#"{{#parseJson "o"}}{"a": {"b": [5, 6]}}{{/parseJson}}{{o.a.b.[1]}} {{jsonPath '{"x": {"y": 9}}' '$.x.y'}} {{request.pathSegments.[1]}}"#, &sc("")).unwrap();
        assert_eq!(j, "6 9 v2");
        let r = render(r#"{{~regexExtract request.body "(?s)locations: (\[.*\]),options" "rl" default="[]"~}}{{rl.0}}|{{regexExtract 'abc123' '[0-9]+'}}"#, &sc("locations: [1,2],options")).unwrap();
        assert_eq!(r, "[1,2]|123");
    }

    /// What WireMock 3.13.1 answers for the same templates (POST /probe/x?case=1&c=1&l=p&l=q&h=a%26b, body f=1&k=v).
    #[test]
    fn wiremock_compat() {
        let mut m = Map::new();
        m.insert("raw".into(), json!("f=1&k=a%26b+c&m=1&m=2"));
        m.insert("__path".into(), json!("/probe/x"));
        m.insert("__query".into(), json!("case=1&c=1&l=p&l=q&h=a%26b"));
        m.insert("query".into(), json!({"case": "1", "c": "1", "l": "q", "h": "a&b"}));
        m.insert("headers".into(), json!({"x-foo": "bar", "host": "localhost:8028"}));
        m.insert("__method".into(), json!("POST"));
        let r = |t: &str| render(t, &m).unwrap_or_else(|e| format!("ERR {e}"));
        for (t, want) in [
            ("{{#eq request.query.c.[0] '1'}}ONE{{else}}OTHER{{/eq}}", "ONE"),
            ("{{#if (eq request.query.c.[0] '1')}}ONE{{else}}OTHER{{/if}}", "ONE"),
            ("{{#with request.query}}{{c.[0]}}|{{c}}{{/with}}", "1|1"),
            ("{{#each request.query.l}}<{{this}}>{{/each}}", "<p><q>"),
            ("{{substring 'abcdef' 1 3}}|{{substring 'abcdef' 4}}", "bc|ef"),
            ("{{#contains 'abc' 'b'}}YES{{else}}NO{{/contains}}", "YES"),
            ("{{#contains request.query.l 'q'}}S{{else}}N{{/contains}}", "S"),
            ("{{request.query.c.[0]}}|{{request.query.c}}|{{request.query.l}}|{{request.query.l.[1]}}|{{size request.query.l}}", "1|1|p|q|2"),
            ("{{request.query.h}}|{{{request.query.h}}}|{{upper request.query.l}}", "a&b|a&b|P"),
            ("{{request.headers.X-Foo}}|{{request.headers.X-Foo.[0]}}|{{#eq request.headers.X-Foo 'bar'}}S{{else}}N{{/eq}}", "bar|bar|N"),
            ("{{request.url}}", "/probe/x?case=1&c=1&l=p&l=q&h=a%26b"),
            ("{{formData request.body 'f'}}{{f.k}}|{{f.m}}|{{f.m.[1]}}", "a%26b+c|1|2"),
            ("{{formData request.body 'g' urlDecode=true}}{{g.k}}", "a&b c"),
            ("{{#formData request.body 'g'}}{{/formData}}{{g.f}}", "1"),
            ("{{jsonPath '{\"a\": \"<\\\"x\\\"&>\"}' '$.a'}}", "<\"x\"&>"),
            ("{{size (now)}}", "20"),
        ] {
            assert_eq!(r(t), want, "{t}");
        }
        // Variables are template-wide (set and read inside each/with/if); fields of the current item come first.
        let geo = "{{#assign \"texto\"}}{{request.query.l}}{{/assign}}{{#parseJson \"tab\"}}[[\"(?i).*acme.*\", -1.50, 2], [\"(?i)p\", -3.25, 4]]{{/parseJson}}{{#assign \"achou\"}}{{/assign}}{{#each tab}}{{#unless achou}}{{#if (matches ../texto this.[0])}}{{#assign \"achou\"}}{{@index}}{{/assign}}{{/if}}{{/unless}}{{/each}}{{#if achou}}{{#with (lookup tab (math achou \"+\" 0))}}{{#assign \"lat\"}}{{this.[1]}}{{/assign}}{{texto}}{{/with}}{{/if}}|{{lat}}|{{math lat \"-\" 0.0013}}";
        assert_eq!(r(geo), "p|-3.25|-3.2513");
        assert_eq!(r("{{#parseJson 'o'}}[{\"v\":\"field\"}]{{/parseJson}}{{#assign 'v'}}var{{/assign}}{{#each o}}{{v}}{{/each}}|{{v}}"), "field|var");
        // matches is a whole-string match; its block renders nothing when false (no else), as WireMock's.
        assert_eq!(r("{{matches 'abc' 'b'}}|{{matches 'abc' 'a.c'}}|{{#matches 'abc' 'b'}}S{{else}}N{{/matches}}|{{#if (matches 'abc' 'b')}}S{{else}}N{{/if}}"), "false|true||N");
        assert_eq!(r("{{#each (range 1 3) as |i|}}<{{i}}:{{@index}}>{{/each}}"), "<1:0><2:1><3:2>");
        // math is BigDecimal arithmetic.
        assert_eq!(r("{{math 0.1 '+' 0.2}}|{{math 10 '/' 4}}|{{math 1.5 '*' 2}}|{{math 1.5 '/' 2}}|{{math 0.0001 '*' 1}}|{{math 12345678.5 '+' 0}}|{{math '' '+' 1}}"), "0.3|3|3.0|0.8|1.0E-4|1.23456785E7|1");
        // parseJson keeps a decimal's text; toJson and an object from jsonPath print as Jackson does.
        assert_eq!(r("{{#parseJson 'p'}}[[-1.50,2.50e1]]{{/parseJson}}{{p.0.0}}|{{p.0.1}}|{{toJson p}}"), "-1.50|25.0|[ [ -1.50, 25.0 ] ]");
        assert_eq!(r("{{jsonPath '{\"a\":{\"z\":1,\"b\":[1,2]}}' '$.a'}}"), "{\n  \"z\" : 1,\n  \"b\" : [ 1, 2 ]\n}");
        // Handlebars.java keeps standalone lines, and ~ does not trim the text before the first or after the last tag.
        assert_eq!(r("A\n{{~#if true~}}\n Y \n{{~/if~}}\nB"), "A\nY\nB");
        assert_eq!(r("{{request.method}}\n{{#if true}}\nY\n{{/if}}\n{{request.method}}"), "POST\n\nY\n\nPOST");
        assert_eq!(r("{{!-- note --}}\n{\"a\": 1}"), "\n{\"a\": 1}");
        // A parseJson that is not JSON is a template error (WireMock answers 500).
        assert!(r("{{#parseJson 'x'}}not json{{/parseJson}}").starts_with("ERR"));
        assert!(r("{{randomValue length=40 type='ALPHANUMERIC'}}").chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        // request.baseUrl, scheme, host and port as WireMock gives them (checked with Host: localhost:18084,
        // acme.example.com and acme.example.com:9999).
        let req_line = "{{request.baseUrl}}|{{request.scheme}}|{{request.host}}|{{request.port}}";
        assert_eq!(r(req_line), "http://localhost:8028|http|localhost|8028");
        for (host, scheme, want) in [("acme.example.com", "http", "http://acme.example.com|http|acme.example.com|80"), ("acme.example.com:9999", "http", "http://acme.example.com:9999|http|acme.example.com|9999"),
            ("acme.example.com:443", "https", "https://acme.example.com|https|acme.example.com|443")] {
            let mut m2 = m.clone();
            m2.insert("headers".into(), json!({"host": host}));
            m2.insert("__scheme".into(), json!(scheme));
            assert_eq!(render(req_line, &m2).unwrap(), want);
        }
        // WireMock has no split helper: a template error there too.
        assert!(r("{{split 'a;b' ';'}}").starts_with("ERR"));
        assert_eq!(bare_values("{{{request.query.h}}}"), "{{__out request.query.h}}");
    }
}

#[cfg(test)]
mod probe {
    /// HBS_SCAN=<folder of plugin YAMLs>: renders every handlebars answer with a synthetic request and lists errors.
    #[test]
    fn scan_folder() {
        let Ok(dir) = std::env::var("HBS_SCAN") else { return };
        let mut sc = serde_json::Map::new();
        sc.insert("raw".into(), serde_json::json!(std::env::var("HBS_BODY").unwrap_or("{}".into())));
        sc.insert("__path".into(), serde_json::json!("/x"));
        sc.insert("__query".into(), serde_json::json!(std::env::var("HBS_QUERY").unwrap_or("q=Acme Street 1, Springfield".into())));
        sc.insert("headers".into(), serde_json::json!({"host": "localhost", "content-type": "application/json"}));
        let mut bad = 0;
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let Ok(p) = crate::plugin::parse(&std::fs::read_to_string(e.path()).unwrap_or_default()) else { continue };
            for a in p.answers.iter().filter(|a| a.template == "handlebars") {
                if let Err(err) = super::render(&a.body, &sc) {
                    bad += 1;
                    println!("{} {} -> {}", p.service.name, a.name, err.lines().next().unwrap_or(""));
                }
            }
        }
        println!("{bad} errors");
    }

    #[test]
    fn probe_file() {
        let Ok(p) = std::env::var("HBS_PROBE") else { return };
        let t = std::fs::read_to_string(p).unwrap();
        if let Err(e) = super::check(&t) {
            panic!("{e}");
        }
    }
}
