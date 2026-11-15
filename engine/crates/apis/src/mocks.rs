//! A11: services from mocks you already have. A WireMock folder (mappings/*.json and __files/)
//! becomes plugins: each request pattern a route (a regex when the mock used one), each mapping a
//! rule and an answer, scenarios a switch (set_scenario), faults and delays the generic answers.
//! A folder of WSDLs becomes one SOAP service per file.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Tool};

use crate::plugin::{self, Answer, Decide, Plugin, Route, Service};

/// One mapping file can hold one mapping or {"mappings": [...]}.
fn mappings_of(v: Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a,
        Value::Object(ref o) if o.contains_key("mappings") => o["mappings"].as_array().cloned().unwrap_or_default(),
        Value::Object(_) => vec![v],
        _ => vec![],
    }
}

fn q(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A WireMock matcher on a value expression → a Stuntman condition (None: cannot express it).
fn matcher(target: &str, m: &Value) -> Option<String> {
    let o = m.as_object()?;
    let mut out = vec![];
    for (k, v) in o {
        let s = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
        out.push(match k.as_str() {
            "equalTo" => format!("{target} == {}", q(&s)),
            "contains" => format!("string({target}).contains({})", q(&s)),
            "doesNotContain" => format!("!string({target}).contains({})", q(&s)),
            "matches" => format!("matches(string({target}), {})", q(&s)),
            "doesNotMatch" => format!("!matches(string({target}), {})", q(&s)),
            "absent" => if v.as_bool() == Some(true) { format!("{target} == null") } else { format!("{target} != null") },
            "caseInsensitive" | "ignoreArrayOrder" | "ignoreExtraElements" => continue,
            _ => return None,
        });
    }
    if out.is_empty() { None } else { Some(out.join(" && ")) }
}

/// "$.a.b" / "$.a[0]" / "$[?(@.x == 'y')]" → a condition on body.
fn json_path(p: &str) -> Option<String> {
    let p = p.trim();
    if let Some(f) = p.strip_prefix("$[?(@.").and_then(|r| r.strip_suffix(")]")) {
        let (field, rest) = f.split_once(|c: char| c == ' ' || c == '=' || c == '!').unwrap_or((f, ""));
        let rest = rest.trim();
        return Some(if rest.is_empty() { format!("body.{field} != null") } else { format!("body.{field} {}", rest.replace('"', "'")) });
    }
    let path = p.strip_prefix("$.")?;
    if path.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '[' || c == ']') {
        return Some(format!("body.{path} != null"));
    }
    None
}

/// XPath forms the mocks use: //*[local-name()='x' and text()='y'], //x, //x[text()='y'].
fn xpath(p: &str) -> Option<String> {
    let re_ln = regex::Regex::new(r#"local-name\(\)\s*=\s*['"]([\w.-]+)['"]"#).unwrap();
    let re_txt = regex::Regex::new(r#"text\(\)\s*=\s*['"]([^'"]*)['"]"#).unwrap();
    let name = re_ln.captures(p).map(|c| c[1].to_string()).or_else(|| {
        let last = p.trim_start_matches('/').split('/').last()?.split('[').next()?.to_string();
        let last = last.split(':').last()?.to_string();
        (!last.is_empty() && last.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.')).then_some(last)
    })?;
    let el = format!("<([\\w.-]+:)?{}[\\s/>]", regex::escape(&name));
    Some(match re_txt.captures(p) {
        Some(t) => format!("matches(raw, {})", q(&format!("(?s)<([\\w.-]+:)?{}[^>]*>\\s*{}\\s*</", regex::escape(&name), regex::escape(&t[1])))),
        None => format!("matches(raw, {})", q(&el)),
    })
}

/// What one request block asks: method, route path and the conditions.
struct Want {
    methods: Vec<String>,
    path: String,
    conds: Vec<String>,
    gaps: Vec<String>,
    host: String,
}

fn want(rq: &Value) -> Want {
    let mut w = Want { methods: vec![], path: String::new(), conds: vec![], gaps: vec![], host: String::new() };
    let m = rq["method"].as_str().unwrap_or("ANY").to_uppercase();
    w.methods = if m == "ANY" { vec!["GET".into(), "POST".into()] } else { vec![m] };
    let is_plain = |s: &str| !s.chars().any(|c| "()[]{}*+?|^$\\".contains(c)) && !s.contains(".*");
    if let Some(u) = rq["urlPath"].as_str() {
        w.path = u.to_string();
    } else if let Some(u) = rq["urlPathTemplate"].as_str() {
        w.path = u.to_string();
    } else if let Some(u) = rq["urlPathPattern"].as_str() {
        w.path = if is_plain(u) { u.to_string() } else { format!("~{u}") };
    } else if let Some(u) = rq["url"].as_str() {
        let (p, qs) = u.split_once('?').unwrap_or((u, ""));
        w.path = p.to_string();
        for kv in qs.split('&').filter(|x| !x.is_empty()) {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            w.conds.push(format!("query[{}] == {}", q(k), q(v)));
        }
    } else if let Some(u) = rq["urlPattern"].as_str() {
        let p = u.split("\\?").next().unwrap_or(u);
        w.path = if is_plain(p) { p.to_string() } else { format!("~{p}") };
        if p != u {
            w.gaps.push(format!("urlPattern {u}: the query part is not checked"));
        }
    } else {
        w.path = "~.*".into();
    }
    if let Some(h) = rq["host"].as_object().and_then(|h| h.get("equalTo")).and_then(|v| v.as_str()).or(rq["host"].as_str()) {
        w.host = h.to_string();
    }
    for (k, m) in rq["queryParameters"].as_object().cloned().unwrap_or_default() {
        match matcher(&format!("query[{}]", q(&k)), &m) {
            Some(c) => w.conds.push(c),
            None => w.gaps.push(format!("query {k}: {m}")),
        }
    }
    for (k, m) in rq["headers"].as_object().cloned().unwrap_or_default() {
        match matcher(&format!("headers[{}]", q(&k.to_lowercase())), &m) {
            Some(c) => w.conds.push(c),
            None => w.gaps.push(format!("header {k}: {m}")),
        }
    }
    for b in rq["bodyPatterns"].as_array().cloned().unwrap_or_default() {
        let Some((k, v)) = b.as_object().and_then(|o| o.iter().find(|(k, _)| !["ignoreArrayOrder", "ignoreExtraElements", "xPathNamespaces"].contains(&k.as_str())).map(|(k, v)| (k.clone(), v.clone()))) else { continue };
        let s = v.as_str().map(String::from).unwrap_or_else(|| v.to_string());
        let c = match k.as_str() {
            "contains" => Some(format!("raw.contains({})", q(&s))),
            "doesNotContain" => Some(format!("!raw.contains({})", q(&s))),
            "matches" => Some(format!("matches(raw, {})", q(&s))),
            "doesNotMatch" => Some(format!("!matches(raw, {})", q(&s))),
            "equalTo" => Some(format!("raw == {}", q(&s))),
            "equalToJson" => {
                let j: Value = if v.is_string() { serde_json::from_str(&s).unwrap_or(Value::Null) } else { v.clone() };
                j.as_object().map(|o| o.iter().filter(|(_, x)| !x.is_object() && !x.is_array()).map(|(f, x)| format!("body.{f} == {}", if x.is_string() { q(x.as_str().unwrap()) } else { x.to_string() })).collect::<Vec<_>>().join(" && ")).filter(|c| !c.is_empty())
            }
            "matchesJsonPath" => if v.is_string() { json_path(&s) } else { None },
            "matchesXPath" | "equalToXml" => if v.is_string() && k == "matchesXPath" { xpath(&s) } else { None },
            _ => None,
        };
        match c {
            Some(c) => w.conds.push(c),
            None => w.gaps.push(format!("body {k}: {}", s.chars().take(80).collect::<String>())),
        }
    }
    if rq.get("multipartPatterns").is_some() {
        w.gaps.push("multipartPatterns are not checked".into());
    }
    w
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    out.trim_matches('_').chars().take(40).collect()
}

pub struct Group {
    pub name: String,
    pub maps: Vec<(PathBuf, Value)>,
}

/// One plugin from a group of mappings. Returns it, its files and what could not be expressed.
pub fn plugin_of(g: &Group, files_root: &Path) -> (Plugin, Vec<(String, Vec<u8>)>, Vec<String>) {
    let mut p = Plugin { service: Service { name: g.name.clone(), title: g.name.clone(), origin: "yours".into(), description: "Imported from WireMock mappings.".into(), ..Default::default() }, ..Default::default() };
    let mut files = vec![];
    let mut gaps = vec![];
    let mut hosts = std::collections::BTreeSet::new();
    // route → [(priority, specific, Decide)]
    let mut routes: BTreeMap<String, Vec<(i64, bool, Decide)>> = BTreeMap::new();
    let mut names = std::collections::HashSet::new();
    let mut maps = g.maps.clone();
    maps.sort_by_key(|(_, m)| m["priority"].as_i64().unwrap_or(5));
    for (file, m) in &maps {
        let rq = &m["request"];
        let rs = &m["response"];
        let w = want(rq);
        if !w.host.is_empty() {
            hosts.insert(w.host.clone());
        }
        for x in &w.gaps {
            gaps.push(format!("{}: {x}", file.file_name().and_then(|f| f.to_str()).unwrap_or("")));
        }
        // The answer.
        let state = m["requiredScenarioState"].as_str().unwrap_or("");
        let base = m["name"].as_str().map(slug).filter(|s| !s.is_empty()).unwrap_or_else(|| {
            let st = rs["status"].as_u64().unwrap_or(200);
            let tail = slug(w.path.trim_start_matches('~').rsplit('/').find(|s| !s.is_empty() && !s.contains(['(', '*', '.'])).unwrap_or("answer"));
            if !state.is_empty() && state != "Started" { format!("{}_{}", slug(state), tail) } else { format!("{tail}_{st}") }
        });
        let mut name = base.clone();
        let mut i = 2;
        while !names.insert(name.clone()) {
            name = format!("{base}_{i}");
            i += 1;
        }
        let fault = rs["fault"].as_str().map(|f| match f { "EMPTY_RESPONSE" => "empty", _ => "reset" });
        let answer_name = match fault {
            Some(f) => f.to_string(),
            None => {
                let mut a = Answer { name: name.clone(), status: rs["status"].as_u64().unwrap_or(200) as u16, ..Default::default() };
                for (k, v) in rs["headers"].as_object().cloned().unwrap_or_default() {
                    a.headers.insert(k.to_lowercase(), match v { Value::Array(x) => x.iter().filter_map(|y| y.as_str()).collect::<Vec<_>>().join(", "), Value::String(x) => x, x => x.to_string() });
                }
                // WireMock with --global-response-templating renders every body; Stuntman renders
                // the ones that have {{ }} as WireMock templates.
                let tpl = |b: &str| rs["transformers"].as_array().map(|t| t.iter().any(|x| x == "response-template")).unwrap_or(false) || b.contains("{{");
                if let Some(b) = rs["body"].as_str() {
                    a.body = b.to_string();
                    if tpl(b) { a.template = "handlebars".into() } else { a.raw = true }
                } else if rs.get("jsonBody").is_some() {
                    let b = serde_json::to_string_pretty(&rs["jsonBody"]).unwrap_or_default();
                    if tpl(&b) { a.template = "handlebars".into() } else { a.raw = true }
                    a.body = b;
                } else if let Some(b) = rs["base64Body"].as_str() {
                    use base64::Engine;
                    let bytes = base64::engine::general_purpose::STANDARD.decode(b).unwrap_or_default();
                    let f = format!("{name}.bin");
                    files.push((f.clone(), bytes));
                    a.file = f;
                } else if let Some(f) = rs["bodyFileName"].as_str() {
                    match std::fs::read(files_root.join(f)) {
                        Ok(bytes) => {
                            let text = String::from_utf8(bytes.clone()).ok();
                            match text.filter(|t| t.len() < 512 * 1024) {
                                Some(t) => {
                                    if tpl(&t) { a.template = "handlebars".into() } else { a.raw = true }
                                    a.body = t;
                                }
                                None => {
                                    let fname = Path::new(f).file_name().and_then(|x| x.to_str()).unwrap_or("file").to_string();
                                    files.push((fname.clone(), bytes));
                                    a.file = fname;
                                }
                            }
                        }
                        Err(_) => gaps.push(format!("{}: __files/{f} not found", file.display())),
                    }
                }
                if let Some(d) = rs["fixedDelayMilliseconds"].as_u64() {
                    a.latency = format!("{d}ms");
                } else if let Some(d) = rs["delayDistribution"]["median"].as_u64() {
                    a.latency = format!("{d}ms");
                }
                if !state.is_empty() && state != "Started" {
                    a.note = format!("scenario {state}");
                }
                p.answers.push(a);
                name
            }
        };
        let mut conds = w.conds.clone();
        let specific = !state.is_empty() && state != "Started";
        if specific {
            conds.insert(0, format!("scenario == {}", q(state)));
        }
        let d = Decide { when: if conds.is_empty() { None } else { Some(conds.join(" && ")) }, answer: answer_name, scenario: m["newScenarioState"].as_str().unwrap_or("").to_string() };
        let prio = m["priority"].as_i64().unwrap_or(5);
        for meth in &w.methods {
            routes.entry(format!("{meth} {}", w.path)).or_default().push((prio, specific, d.clone()));
        }
    }
    let mut prio: Vec<i64> = vec![];
    for (r, mut ds) in routes {
        let min_prio = ds.iter().map(|d| d.0).min().unwrap_or(5);
        // Scenario-specific rules first, then by priority; the first rule with no condition ends the list.
        ds.sort_by_key(|(prio, specific, d)| (!*specific, d.when.is_none(), *prio));
        let mut decide: Vec<Decide> = vec![];
        for (_, _, d) in ds {
            let last = d.when.is_none();
            decide.push(d);
            if last {
                break;
            }
        }
        // Nothing matched: WireMock's 404.
        if decide.last().map(|d| d.when.is_some()).unwrap_or(true) {
            decide.push(Decide { answer: "not_matched".into(), ..Default::default() });
            if !p.answers.iter().any(|a| a.name == "not_matched") {
                p.answers.push(Answer { name: "not_matched".into(), status: 404, headers: [("content-type".to_string(), "text/plain".to_string())].into(), body: "Request was not matched".into(), raw: true, note: "no mapping matched (as WireMock)".into(), ..Default::default() });
            }
        }
        p.routes.push(Route { route: r, decide, fallthrough: true, ..Default::default() });
        prio.push(min_prio);
    }
    // Plain paths before regexes (/a/b before /a/.*), then by WireMock priority.
    let mut idx: Vec<usize> = (0..p.routes.len()).collect();
    idx.sort_by_key(|i| (p.routes[*i].method_path().1.starts_with('~'), prio[*i]));
    p.routes = idx.into_iter().map(|i| p.routes[i].clone()).collect();
    if hosts.len() == 1 {
        p.service.real_host = hosts.into_iter().next().unwrap_or_default();
    }
    (p, files, gaps)
}

/// Mapping files of a folder (or of its mappings/ subfolder), grouped by subfolder, host or as one.
pub fn read_folder(root: &Path, group_by: &str, one_name: &str) -> Result<(Vec<Group>, PathBuf), ApiError> {
    let maps_dir = if root.join("mappings").is_dir() { root.join("mappings") } else { root.to_path_buf() };
    let files_root = if root.join("__files").is_dir() { root.join("__files") } else { maps_dir.join("__files") };
    if !maps_dir.is_dir() {
        return Err(ApiError::bad(format!("{} is not a folder", root.display())));
    }
    let mut all: Vec<(PathBuf, Value)> = vec![];
    let mut stack = vec![maps_dir.clone()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| ApiError::bad(format!("{}: {e}", d.display())))?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().map(|n| n != "__files").unwrap_or(true) {
                    stack.push(p);
                }
            } else if p.extension().map(|x| x == "json").unwrap_or(false) {
                let v: Value = std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
                for m in mappings_of(v) {
                    if m.get("request").is_some() && m.get("response").is_some() {
                        all.push((p.clone(), m));
                    }
                }
            }
        }
    }
    let root_name = slug(root.file_name().and_then(|n| n.to_str()).unwrap_or("mocks")).replace('_', "-");
    let mut groups: BTreeMap<String, Vec<(PathBuf, Value)>> = BTreeMap::new();
    for (p, m) in all {
        let key = match group_by {
            "one" => if one_name.is_empty() { root_name.clone() } else { one_name.to_string() },
            "host" => m["request"]["host"].as_object().and_then(|h| h.get("equalTo")).and_then(|v| v.as_str()).or(m["request"]["host"].as_str()).map(|h| h.split('.').next().unwrap_or(h).to_string()).unwrap_or(root_name.clone()),
            _ => p.parent().filter(|d| *d != maps_dir).and_then(|d| d.strip_prefix(&maps_dir).ok()).and_then(|r| r.components().next()).and_then(|c| c.as_os_str().to_str()).map(String::from).unwrap_or(root_name.clone()),
        };
        groups.entry(slug(&key).replace('_', "-")).or_default().push((p, m));
    }
    Ok((groups.into_iter().map(|(name, maps)| Group { name, maps }).collect(), files_root))
}

fn wsdls(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("wsdl")).unwrap_or(false) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("import_mocks", "Services from mocks you already have. kind=wiremock: a folder with mappings/*.json (and __files/) becomes plugins: request patterns → routes (regex paths kept), matchers → rules, responses → answers, fixedDelay → latency, faults → reset/empty, scenarios → set_scenario. kind=wsdl: every .wsdl under the folder becomes a SOAP service. dry=true only shows what it would make.", |ctx, a| async move {
            let src = a.need("source")?;
            let root = PathBuf::from(shellexpand(&src));
            let kind = a.str("kind").unwrap_or_else(|| if wsdls(&root).is_empty() || root.join("mappings").is_dir() { "wiremock".into() } else { "wsdl".into() });
            let dry = a.bool("dry").unwrap_or(false);
            let replace = a.bool("replace").unwrap_or(false);
            let existing: Vec<String> = plugin::all(&ctx).iter().map(|p| p.service.name.clone()).collect();
            let mut out = vec![];
            if kind == "wsdl" {
                for w in wsdls(&root) {
                    let name = slug(w.file_stem().and_then(|s| s.to_str()).unwrap_or("service")).replace('_', "-");
                    if existing.contains(&name) && !replace {
                        out.push(json!({"service": name, "skipped": "exists (replace=true to overwrite)"}));
                        continue;
                    }
                    if dry {
                        out.push(json!({"service": name, "wsdl": w.display().to_string()}));
                        continue;
                    }
                    match crate::soap::import(&ctx.http, &name, &w.display().to_string(), None).await {
                        Ok(im) => match crate::soap::save(&ctx, im, None) {
                            Ok(v) => out.push(json!({"service": name, "operations": v["operations"].as_array().map(|x| x.len()).unwrap_or(0)})),
                            Err(e) => out.push(json!({"service": name, "error": e.message})),
                        },
                        Err(e) => out.push(json!({"service": name, "error": e.message})),
                    }
                }
                return Ok(json!({"kind": "wsdl", "source": src, "services": out}));
            }
            let group_by = a.str("group_by").unwrap_or("folder".into());
            let (groups, files_root) = read_folder(&root, &group_by, &a.str("name").unwrap_or_default())?;
            let mut total = (0, 0);
            for g in &groups {
                let (p, files, gaps) = plugin_of(g, &files_root);
                total.0 += g.maps.len();
                total.1 += p.routes.len();
                let mut row = json!({"service": p.service.name, "mappings": g.maps.len(), "routes": p.routes.len(), "answers": p.answers.len(),
                    "faults": p.routes.iter().flat_map(|r| r.decide.iter()).filter(|d| ["reset", "empty"].contains(&d.answer.as_str())).count(),
                    "scenarios": scenarios(&p),
                    "files": files.len(), "gaps": gaps});
                if existing.contains(&p.service.name) && !replace {
                    row["skipped"] = json!("exists (replace=true to overwrite)");
                } else if !dry {
                    let text = serde_yaml::to_string(&p).map_err(|e| ApiError::internal(e.to_string()))?;
                    if let Some(e) = plugin::validate(&p, &text).first() {
                        row["error"] = e["error"].clone();
                    } else {
                        let dir = crate::record::files_dir(&ctx, &p.service.name);
                        if !files.is_empty() {
                            std::fs::create_dir_all(&dir).map_err(|e| ApiError::internal(e.to_string()))?;
                        }
                        for (f, b) in &files {
                            std::fs::write(dir.join(f), b).map_err(|e| ApiError::internal(e.to_string()))?;
                        }
                        plugin::save(&ctx, &p)?;
                    }
                }
                out.push(row);
            }
            Ok(json!({"kind": "wiremock", "source": src, "groupBy": group_by, "mappings": total.0, "routes": total.1, "services": out, "dry": dry}))
        })
        .req("source", "a folder on the machine that runs Stuntman").param("kind", "wiremock | wsdl (found by the folder)").param("group_by", "folder (one service per subfolder of mappings/) | host | one")
        .param("name", "with group_by=one: the service name").param("dry", "bool: only show what it would make").param("replace", "bool: overwrite services that exist")
        .route("POST", "/plugins/import"),
        Tool::new("set_scenario", "Moves a service to a scenario (imported WireMock scenarios: rules with `scenario == 'x'`). Started is the normal one.", |ctx, a| async move {
            let s = a.need("service")?;
            plugin::get(&ctx, &s)?;
            let name = a.str("scenario").unwrap_or("Started".into());
            crate::store::keep(&ctx, &s, "__scenario", "state", json!({"name": name, "at": util::now_ms()})).await?;
            ctx.live.send("apis.scenario", &s, "updated");
            let p = plugin::get(&ctx, &s)?;
            Ok(json!({"service": s, "scenario": name, "scenarios": scenarios(&p)}))
        })
        .req("service", "plugin").param("scenario", "the scenario's name (Started: normal)")
        .route("PUT", "/plugins/{service}/scenario"),
    ]
}

/// The scenarios a plugin's rules name (scenario == 'x').
pub fn scenarios(p: &Plugin) -> std::collections::BTreeSet<String> {
    let re = regex::Regex::new(r"scenario == '([^']+)'").unwrap();
    p.routes.iter().flat_map(|r| r.decide.iter()).filter_map(|d| d.when.as_ref()).flat_map(|w| re.captures_iter(w).map(|c| c[1].to_string()).collect::<Vec<_>>()).collect()
}

fn shellexpand(s: &str) -> String {
    match s.strip_prefix("~/") {
        Some(r) => format!("{}/{r}", std::env::var("HOME").unwrap_or_default()),
        None => s.to_string(),
    }
}


#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn matchers_become_conditions() {
        let w = want(&json!({"method": "POST", "urlPathPattern": "(?i)(/acme)?/ws/track\\.asmx", "headers": {"SOAPAction": {"contains": "/Track"}},
            "bodyPatterns": [{"matchesXPath": "//*[local-name()='cUF' and text()='35']"}, {"matchesJsonPath": "$.img"}, {"contains": "abc"}]}));
        assert_eq!(w.path, "~(?i)(/acme)?/ws/track\\.asmx");
        assert_eq!(w.conds[0], "string(headers['soapaction']).contains('/Track')");
        assert!(w.conds[1].starts_with("matches(raw, '(?s)<([\\\\w.-]+:)?cUF"), "{}", w.conds[1]);
        assert_eq!(w.conds[2], "body.img != null");
        assert_eq!(w.conds[3], "raw.contains('abc')");
        let w = want(&json!({"method": "GET", "url": "/v1/x?a=1&b=2"}));
        assert_eq!((w.path.as_str(), w.conds.len()), ("/v1/x", 2));
    }
}
