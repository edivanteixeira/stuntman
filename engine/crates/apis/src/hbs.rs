//! WireMock response templates (A11): answers with `template: handlebars` render like WireMock's
//! response-template transformer, so imported mocks keep working. The request is `request` (url,
//! path, pathSegments, query, headers, body, method, baseUrl); the helpers are WireMock's: now,
//! randomValue, randomInt, math, numberFormat, xPath, jsonPath, regexExtract, parseJson, assign,
//! size, toJson, trim, replace, contains and the comparisons.

use handlebars::{Context, Handlebars, Helper, HelperDef, HelperResult, Output, RenderContext, RenderError, RenderErrorReason, Renderable, ScopedJson, StringOutput};
use serde_json::{json, Map, Value};

type F = fn(&[Value], &Map<String, Value>) -> Result<Value, String>;

struct V(F);

impl HelperDef for V {
    fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, _: &'rc Context, _: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
        let args: Vec<Value> = h.params().iter().map(|p| p.value().clone()).collect();
        let hash: Map<String, Value> = h.hash().iter().map(|(k, v)| (k.to_string(), v.value().clone())).collect();
        (self.0)(&args, &hash).map(ScopedJson::Derived).map_err(|e| RenderErrorReason::Other(format!("{}: {e}", h.name())).into())
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
                buf.into_string().unwrap_or_default()
            }
        };
        let val = if self.0 {
            // A block renders escaped ({{request.body}} → &quot;): parse what it meant.
            let raw = text.trim().replace("&quot;", "\"").replace("&#x27;", "'").replace("&#x3D;", "=").replace("&#x60;", "`").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
            serde_json::from_str(&raw).unwrap_or(Value::Null)
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
        let mut data = rc.context().map(|c| c.data().clone()).unwrap_or_else(|| ctx.data().clone());
        if let Some(o) = data.as_object_mut() {
            o.insert(name, val);
        }
        rc.set_context(Context::wraps(data)?);
        Ok(())
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
                let mut data = rc.context().map(|c| c.data().clone()).unwrap_or_else(|| ctx.data().clone());
                if let Some(o) = data.as_object_mut() {
                    o.insert(var, val);
                }
                rc.set_context(Context::wraps(data)?);
                Ok(())
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
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

fn n(v: &Value) -> f64 {
    match v {
        Value::Number(x) => x.as_f64().unwrap_or(0.0),
        Value::String(x) => x.trim().replace(',', ".").parse().unwrap_or(0.0),
        Value::Bool(b) => *b as i64 as f64,
        _ => 0.0,
    }
}

fn num(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() < 9e15 { json!(x as i64) } else { json!(x) }
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
    let set: &[u8] = match ty.as_str() {
        "NUMERIC" => b"0123456789",
        "ALPHABETIC" => b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "HEXADECIMAL" => b"0123456789abcdef",
        "ALPHANUMERIC_AND_SYMBOLS" => b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#$%^&*",
        _ => b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
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

fn math(a: &[Value], _: &Map<String, Value>) -> Result<Value, String> {
    let (x, op, y) = (n(&arg(a, 0)), s(&arg(a, 1)), n(&arg(a, 2)));
    Ok(num(match op.as_str() {
        "+" => x + y,
        "-" => x - y,
        "*" => x * y,
        "/" => if y == 0.0 { 0.0 } else { x / y },
        "%" => if y == 0.0 { 0.0 } else { x % y },
        o => return Err(format!("operator {o}")),
    }))
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

/// //*[local-name()='x']/text(), //x/text(), //x/@attr, count(//x): the forms mocks use.
pub fn xpath_eval(xml: &str, expr: &str) -> Value {
    let Ok(doc) = roxmltree::Document::parse(xml) else { return Value::Null };
    let e = expr.trim();
    let (count, e) = match e.strip_prefix("count(").and_then(|x| x.strip_suffix(')')) { Some(x) => (true, x), None => (false, e) };
    let ln = regex::Regex::new(r#"local-name\(\)\s*=\s*['"]([\w.-]+)['"]"#).unwrap();
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
    let re = regex::Regex::new(r"\.?([\w-]+)|\[(\d+)\]|\['([^']+)'\]").unwrap();
    for c in re.captures_iter(p) {
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
    Ok(json!(arg(a, 0).to_string()))
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

fn registry(now: i64) -> Handlebars<'static> {
    let mut hb = Handlebars::new();
    hb.set_strict_mode(false);
    let list: [(&str, F); 25] = [("randomValue", random_value), ("randomInt", random_int), ("math", math), ("numberFormat", number_format), ("xPath", xpath_h),
        ("jsonPath", json_path_h), ("size", size), ("toJson", to_json), ("trim", trim), ("replace", replace), ("contains", contains),
        ("eq", eq), ("ne", ne), ("neq", ne), ("lt", lt), ("gt", gt), ("lte", lte), ("gte", gte), ("and", and), ("or", or), ("not", not), ("upper", upper), ("lower", lower),
        ("base64", base64_h), ("now", now_h)];
    for (k, f) in list {
        hb.register_helper(k, Box::new(V(f)));
    }
    // now() reads the environment's clock.
    struct Now(i64);
    impl HelperDef for Now {
        fn call_inner<'reg: 'rc, 'rc>(&self, h: &Helper<'rc>, _: &'reg Handlebars<'reg>, _: &'rc Context, _: &mut RenderContext<'reg, 'rc>) -> Result<ScopedJson<'rc>, RenderError> {
            let mut hash: Map<String, Value> = h.hash().iter().map(|(k, v)| (k.to_string(), v.value().clone())).collect();
            hash.insert("__now".into(), json!(self.0));
            now_h(&[], &hash).map(ScopedJson::Derived).map_err(|e| RenderErrorReason::Other(e).into())
        }
    }
    hb.register_helper("now", Box::new(Now(now)));
    hb.register_helper("regexExtract", Box::new(RegexExtract));
    hb.register_helper("assign", Box::new(Assign(false)));
    hb.register_helper("parseJson", Box::new(Assign(true)));
    hb
}

/// The `request` WireMock templates read, from Stuntman's scope; header names the template uses
/// (request.headers.SOAPAction) are found whatever their case.
pub fn context(tpl: &str, sc: &Map<String, Value>) -> Value {
    let path = sc.get("__path").and_then(|v| v.as_str()).unwrap_or("/").to_string();
    let query: Map<String, Value> = sc.get("query").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut headers: Map<String, Value> = sc.get("headers").and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let re = regex::Regex::new(r"(?i)request\.headers\.\[?([\w-]+)").unwrap();
    for c in re.captures_iter(tpl) {
        let want = c[1].to_string();
        if let Some(v) = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(&want)).map(|x| x.1.clone()) {
            headers.insert(want, v);
        }
    }
    let qs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", s(v))).collect();
    let host = headers.get("host").map(s).unwrap_or_default();
    let request = json!({
        "url": if qs.is_empty() { path.clone() } else { format!("{path}?{}", qs.join("&")) },
        "path": path, "pathSegments": path.trim_matches('/').split('/').collect::<Vec<_>>(),
        "query": query, "headers": headers, "body": sc.get("raw").cloned().unwrap_or(json!("")),
        "method": sc.get("__method").cloned().unwrap_or(json!("GET")), "baseUrl": format!("http://{host}"), "host": host,
    });
    // Webhooks (postServeActions) read the call that triggered them as originalRequest, and the
    // answer it got as originalResponse.
    json!({"request": request, "originalRequest": request, "originalResponse": sc.get("__response").cloned().unwrap_or(Value::Null)})
}

pub fn render(tpl: &str, sc: &Map<String, Value>) -> Result<String, String> {
    let now = sc.get("__now").and_then(|v| v.as_i64()).unwrap_or_else(stuntman_core::util::now_ms);
    registry(now).render_template(&java_strings(tpl), &context(tpl, sc)).map_err(|e| e.to_string())
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
}

#[cfg(test)]
mod probe {
    #[test]
    fn probe_file() {
        let Ok(p) = std::env::var("HBS_PROBE") else { return };
        let t = std::fs::read_to_string(p).unwrap();
        if let Err(e) = super::check(&t) {
            panic!("{e}");
        }
    }
}
