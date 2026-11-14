//! S2 · Rules: when (operation + account + container + path glob + only when…) → then (response)
//! → how often. One file per service: storage/rules/<service>.yaml. The first enabled rule that
//! matches answers; a rule never touches other environments.

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Cond {
    /// size | content_type | caller
    pub field: String,
    /// size: over | under; text: is | contains | is_not
    pub op: String,
    pub value: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct When {
    /// put | get | delete | list | link | any
    #[serde(default = "any")]
    pub op: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub container: String,
    /// Glob: * one level, ** any depth; empty is the whole container.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only_when: Vec<Cond>,
}
fn any() -> String {
    "any".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Then {
    /// 403 | 404 | 409 | 412 | 413 | 503 | timeout | reset | empty | slow | truncate | bad_md5 | expires
    pub respond: String,
    /// slow: seconds added; expires: link lifetime in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct HowOften {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub random: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Rule {
    pub name: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub when: When,
    pub then: Then,
    #[serde(default)]
    pub how_often: HowOften,
}
fn yes() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct File {
    #[serde(default)]
    pub rules: Vec<Rule>,
}

pub const OPS: [(&str, &str); 6] = [("put", "Upload (PUT)"), ("get", "Download (GET)"), ("delete", "Delete"), ("list", "List"), ("link", "Create read link (SAS)"), ("any", "Any operation")];
pub const RESPONSES: [(&str, &str, &str); 13] = [
    ("Errors", "403", "403 Forbidden"),
    ("Errors", "404", "404 Not found"),
    ("Errors", "409", "409 Already exists"),
    ("Errors", "412", "412 ETag does not match"),
    ("Errors", "413", "413 Too large"),
    ("Errors", "503", "503 Server busy"),
    ("Network", "timeout", "Timeout (no answer)"),
    ("Network", "reset", "Connection reset"),
    ("Network", "empty", "Empty response"),
    ("Content", "slow", "Slow · +N seconds"),
    ("Content", "truncate", "Cut off halfway"),
    ("Content", "bad_md5", "Wrong checksum"),
    ("Links", "expires", "Links expire after N seconds"),
];

impl Rule {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("name: required".into());
        }
        if !OPS.iter().any(|o| o.0 == self.when.op) {
            return Err(format!("when.op: {}", OPS.iter().map(|o| o.0).collect::<Vec<_>>().join(" | ")));
        }
        if !RESPONSES.iter().any(|r| r.1 == self.then.respond) {
            return Err(format!("then.respond: {}", RESPONSES.iter().map(|r| r.1).collect::<Vec<_>>().join(" | ")));
        }
        if matches!(self.then.respond.as_str(), "slow" | "expires") && self.then.seconds.is_none() {
            return Err(format!("then.seconds: how many seconds for {}", self.then.respond));
        }
        if self.how_often.random.map(|p| p == 0 || p > 100).unwrap_or(false) {
            return Err("how_often.random: 1 to 100 (%)".into());
        }
        for c in &self.when.only_when {
            if !["size", "content_type", "caller"].contains(&c.field.as_str()) {
                return Err("only_when.field: size | content_type | caller".into());
            }
            if c.field == "size" && parse_size(&c.value).is_none() {
                return Err("only_when: a size like 10 MB".into());
            }
        }
        Ok(())
    }

    /// "PUT contracts/** → 503×2"
    pub fn meta(&self) -> String {
        let op = if self.when.op == "link" { "SAS".to_string() } else if self.when.op == "any" { "any".into() } else { self.when.op.to_uppercase() };
        let path = if self.when.path.is_empty() { if self.when.container.is_empty() { "any".to_string() } else { format!("{}/**", self.when.container) } } else { self.when.path.clone() };
        let then = match self.then.respond.as_str() {
            "slow" => format!("+{} s", self.then.seconds.unwrap_or(0)),
            "expires" => format!("expires {} s", self.then.seconds.unwrap_or(0)),
            r => r.to_string(),
        };
        let often = match (self.how_often.first, self.how_often.random) {
            (Some(n), _) => format!("×{n}"),
            (_, Some(p)) => format!(" {p}%"),
            _ => String::new(),
        };
        format!("{op} {path} → {then}{often}")
    }
}

pub fn parse_size(s: &str) -> Option<u64> {
    let t = s.trim().to_uppercase().replace(' ', "");
    let n: f64 = t.trim_end_matches(|c: char| c.is_alphabetic()).parse().ok()?;
    let f = if t.ends_with("GB") { 1 << 30 } else if t.ends_with("MB") { 1 << 20 } else if t.ends_with("KB") { 1 << 10 } else { 1 };
    Some((n * f as f64) as u64)
}

/// Glob over "/"-separated paths: * matches inside one level, ** any depth.
pub fn glob(pat: &str, path: &str) -> bool {
    if pat.is_empty() {
        return true;
    }
    let mut re = String::from("^");
    let mut chars = pat.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                re.push_str(".*");
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re).map(|r| r.is_match(path)).unwrap_or(false)
}

/// One request as rules see it.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub op: String,
    pub account: String,
    pub container: String,
    pub path: String,
    pub size: u64,
    pub ctype: String,
    pub caller: String,
}

pub fn matches(r: &Rule, s: &Seen) -> bool {
    let w = &r.when;
    let op_ok = w.op == "any" || w.op == s.op || (w.op == "get" && s.op == "head");
    op_ok
        && (w.account.is_empty() || w.account == s.account)
        && (w.container.is_empty() || w.container == s.container)
        && glob(&w.path, &s.path)
        && w.only_when.iter().all(|c| match c.field.as_str() {
            "size" => {
                let v = parse_size(&c.value).unwrap_or(0);
                if c.op == "under" { s.size < v } else { s.size > v }
            }
            f => {
                let have = if f == "caller" { &s.caller } else { &s.ctype };
                match c.op.as_str() {
                    "contains" => have.contains(&c.value),
                    "is_not" => *have != c.value,
                    _ => *have == c.value,
                }
            }
        })
}

pub fn file_path(ctx: &Ctx, service: &str) -> std::path::PathBuf {
    ctx.path(&format!("storage/rules/{service}.yaml"))
}

pub fn load(ctx: &Ctx, service: &str) -> Result<File, ApiError> {
    stuntman_core::yamlfile::load(&file_path(ctx, service))
}

pub fn save(ctx: &Ctx, service: &str, f: &File) -> Result<(), ApiError> {
    let mut seen = std::collections::HashSet::new();
    for r in &f.rules {
        r.validate().map_err(|e| ApiError::bad(format!("“{}”: {e}", r.name)))?;
        if !seen.insert(r.name.clone()) {
            return Err(ApiError::new(409, format!("Two rules are called “{}”", r.name)));
        }
    }
    stuntman_core::yamlfile::save(&file_path(ctx, service), f)?;
    ctx.live.send("storage.rules", service, "updated");
    Ok(())
}

/// The count of "first N" starts again (on save or switch on).
pub async fn reset_count(ctx: &Ctx, service: &str, name: &str) -> Result<(), ApiError> {
    let (s, n) = (service.to_string(), name.to_string());
    ctx.db.call(move |c| c.execute("DELETE FROM storage_rule_counts WHERE service=?1 AND name=?2", params![s, n]).map(|_| ())).await
}

/// The rule that answers this request, if any fires now: (rule, its Then).
pub async fn decide(ctx: &Ctx, service: &str, s: &Seen) -> Result<Option<Rule>, ApiError> {
    let f = load(ctx, service).unwrap_or_default();
    for r in f.rules.into_iter().filter(|r| r.enabled) {
        if !matches(&r, s) {
            continue;
        }
        let (svc, name) = (service.to_string(), r.name.clone());
        let n: i64 = ctx
            .db
            .call(move |c| {
                c.execute("INSERT INTO storage_rule_counts VALUES(?1,?2,1) ON CONFLICT(service,name) DO UPDATE SET n=n+1", params![svc, name])?;
                c.query_row("SELECT n FROM storage_rule_counts WHERE service=?1 AND name=?2", params![svc, name], |x| x.get(0))
            })
            .await?;
        let fire = match (r.how_often.first, r.how_often.random) {
            (Some(k), _) => n <= k as i64,
            (_, Some(p)) => rand::random::<u32>() % 100 < p,
            _ => true,
        };
        if fire {
            let (svc, name) = (service.to_string(), r.name.clone());
            ctx.db.call(move |c| c.execute("INSERT INTO storage_rule_hits VALUES(?1,?2,?3)", params![svc, name, util::now_ms()]).map(|_| ())).await?;
            return Ok(Some(r));
        }
        // "first N, then normal": a rule that stopped firing lets the request through as normal.
        return Ok(None);
    }
    Ok(None)
}

/// Hits today and last hit per rule.
pub async fn hits(ctx: &Ctx, service: &str) -> Result<std::collections::BTreeMap<String, (i64, Option<i64>)>, ApiError> {
    let s = service.to_string();
    let day = util::now_ms() - 86_400_000;
    ctx.db
        .call(move |c| {
            let mut q = c.prepare("SELECT name, sum(at > ?2), max(at) FROM storage_rule_hits WHERE service=?1 GROUP BY name")?;
            let v = q.query_map(params![s, day], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))))?.collect::<Result<_, _>>();
            v
        })
        .await
}

fn status_text(r: &Then, ok: u16) -> String {
    match r.respond.as_str() {
        "403" => "403 Forbidden".into(),
        "404" => "404 Not found".into(),
        "409" => "409 Already exists".into(),
        "412" => "412 Precondition failed".into(),
        "413" => "413 Too large".into(),
        "503" => "503 Server busy · Retry-After: 1".into(),
        "timeout" => "no answer (the app's timeout)".into(),
        "reset" => "connection reset".into(),
        "empty" => "empty response".into(),
        "slow" => format!("{ok} after +{} s", r.seconds.unwrap_or(0)),
        "truncate" => format!("{ok} · cut off halfway"),
        "bad_md5" => format!("{ok} · wrong Content-MD5"),
        "expires" => format!("403 AuthenticationFailed after {} s", r.seconds.unwrap_or(0)),
        x => x.to_string(),
    }
}

fn lev(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// "Try it": what the draft rule would match among the last requests and what the app sees per
/// attempt; warnings for a rule above that answers first, and the closest near miss.
pub fn test(rule: &Rule, others: &[Rule], last: &[Value]) -> Value {
    let seen = |v: &Value| Seen {
        op: v["op"].as_str().unwrap_or("").into(),
        account: v["account"].as_str().unwrap_or("").into(),
        container: v["container"].as_str().unwrap_or("").into(),
        path: v["path"].as_str().unwrap_or("").into(),
        size: v["size"].as_u64().unwrap_or(0),
        ctype: v["ctype"].as_str().unwrap_or("").into(),
        caller: v["caller"].as_str().unwrap_or("").into(),
    };
    let hits: Vec<&Value> = last.iter().filter(|v| matches(rule, &seen(v))).collect();
    let ok = match rule.when.op.as_str() { "put" => 201, "delete" => 202, _ => 200 };
    let ok_text = match ok { 201 => "201 Created", 202 => "202 Accepted", _ => "200 OK" };
    let tries: Vec<Value> = match (rule.how_often.first, rule.how_often.random) {
        (Some(n), _) => (1..=n.min(4) + 1).map(|i| if i <= n { json!({"try": i, "answer": status_text(&rule.then, ok), "provoked": true}) } else { json!({"try": i, "answer": format!("{ok_text} · back to normal"), "provoked": false}) }).collect(),
        (_, Some(p)) => vec![json!({"try": 0, "answer": format!("about 1 in {} fails: {}", (100 / p.max(1)).max(1), status_text(&rule.then, ok)), "provoked": true})],
        _ => (1..=3).map(|i| json!({"try": i, "answer": status_text(&rule.then, ok), "provoked": true})).collect(),
    };
    let mut warnings = vec![];
    let me = others.iter().position(|r| r.name == rule.name).unwrap_or(others.len());
    for above in others.iter().take(me).filter(|r| r.enabled && r.name != rule.name) {
        let a = &above.when;
        let w = &rule.when;
        let same_where = (a.op == "any" || a.op == w.op) && (a.account.is_empty() || a.account == w.account) && (a.container.is_empty() || a.container == w.container);
        let covers = a.path.is_empty() || a.path == w.path || glob(&a.path, &w.path.replace('*', "x"));
        if same_where && covers && a.only_when.is_empty() && above.how_often.random.is_none() && above.how_often.first.is_none() {
            warnings.push(json!({"kind": "shadowed", "by": above.name, "text": format!("“{}” is above it and matches the same requests, so it answers first.", above.name)}));
        }
    }
    let mut near = Value::Null;
    if hits.is_empty() && !rule.when.path.is_empty() {
        let lit = rule.when.path.split('/').next().unwrap_or("").to_string();
        if !lit.contains('*') && !lit.is_empty() {
            'outer: for v in last {
                let p = v["path"].as_str().unwrap_or("");
                for seg in p.split('/') {
                    let d = lev(&lit, seg);
                    if d > 0 && d <= 2 {
                        let common: String = lit.chars().zip(seg.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a).collect();
                        let fix = rule.when.path.replacen(&lit, &format!("{common}*"), 1);
                        near = json!({"path": format!("{}/{}", v["container"].as_str().unwrap_or(""), p), "here": seg, "yours": lit, "fix": fix});
                        break 'outer;
                    }
                }
            }
        }
    }
    json!({
        "wouldMatch": hits.iter().take(20).map(|v| json!({"path": format!("{}/{}", v["container"].as_str().unwrap_or(""), v["path"].as_str().unwrap_or("")), "method": v["method"], "at": v["at"], "caller": v["caller"]})).collect::<Vec<_>>(),
        "matched": hits.len(), "of": last.len(), "untouched": last.len() - hits.len(),
        "appWillSee": tries, "warnings": warnings, "nearMiss": near,
    })
}

#[cfg(test)]
mod t {
    use super::*;

    fn rule(path: &str) -> Rule {
        Rule { name: "r".into(), enabled: true, when: When { op: "put".into(), path: path.into(), ..Default::default() }, then: Then { respond: "503".into(), seconds: None }, how_often: HowOften { first: Some(2), random: None } }
    }

    #[test]
    fn globs() {
        assert!(glob("contracts/**", "contracts/2026/11/a.pdf"));
        assert!(!glob("contracts/*", "contracts/2026/a.pdf"));
        assert!(glob("contracts/*", "contracts/a.pdf"));
        assert!(glob("", "anything"));
        assert!(glob("*.pdf", "a.pdf"));
    }

    #[test]
    fn try_it_near_miss_and_attempts() {
        let last = vec![json!({"op": "put", "account": "a", "container": "documents", "path": "contract/2026/11/nda.pdf", "method": "PUT"})];
        let v = test(&rule("contracts/**"), &[], &last);
        assert_eq!(v["matched"], 0);
        assert_eq!(v["nearMiss"]["fix"], "contract*/**");
        let v = test(&rule("contract*/**"), &[], &last);
        assert_eq!(v["matched"], 1);
        assert_eq!(v["appWillSee"][2]["answer"], "201 Created · back to normal");
    }

    #[test]
    fn shadow_warning() {
        let mut above = rule("");
        above.name = "all".into();
        above.how_often = HowOften::default();
        let mine = rule("contracts/**");
        let v = test(&mine, &[above, mine.clone()], &[]);
        assert_eq!(v["warnings"][0]["by"], "all");
    }
}
