//! The Catalog (K1–K3): plugin files shipped in engine/plugins (official and community), their
//! collections (_collections.yaml), and imports of your own plugin from a folder, a .zip or a git
//! repository. An import is only read and checked; nothing is installed until install_import.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
};

use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

use crate::{
    expr,
    plugin::{self, Field, Plugin},
};

fn files() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = std::fs::read_dir(plugin::catalog_dir()).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false) && !e.file_name().to_string_lossy().starts_with('_'))
        .filter_map(|e| Some((e.path().file_stem()?.to_string_lossy().to_string(), std::fs::read_to_string(e.path()).ok()?))).collect();
    v.sort();
    v
}

pub fn text(name: &str) -> Result<String, ApiError> {
    plugin::valid_name(name)?;
    std::fs::read_to_string(plugin::catalog_dir().join(format!("{name}.yaml"))).map_err(|_| ApiError::not_found(format!("No plugin “{name}” in the catalog")))
}

fn badges(p: &Plugin) -> Vec<&'static str> {
    let mut b = vec![];
    if !p.pages.is_empty() {
        b.push("browser page");
    }
    if p.service.billing.enabled {
        b.push("billable");
    }
    if p.routes.iter().any(|r| !r.send.is_empty()) {
        b.push("webhooks");
    }
    b
}

/// One card: what it is, where it's from, and whether this environment has it.
fn card(p: &Plugin, t: &str, installed: &HashMap<String, String>) -> Value {
    let mine = installed.get(&p.service.name);
    json!({"name": p.service.name, "title": if p.service.title.is_empty() { p.service.name.clone() } else { p.service.title.clone() }, "origin": p.service.origin,
        "region": p.service.region, "description": p.service.description, "routes": p.routes.len(), "pages": p.pages.len(), "badges": badges(p),
        "version": plugin::version(t), "installed": mine.is_some(), "update": mine.map(|v| v != &plugin::version(t)).unwrap_or(false)})
}

pub fn collections() -> Vec<Value> {
    let t = std::fs::read_to_string(plugin::catalog_dir().join("_collections.yaml")).unwrap_or_default();
    serde_yaml::from_str::<Value>(&t).ok().and_then(|v| v["collections"].as_array().cloned()).unwrap_or_default()
}

/// K1: cards, filtered; categories and collections with what is installed here.
pub fn list(ctx: &Ctx, query: &str, region: &str, category: &str, collection: &str) -> Value {
    let installed: HashMap<String, String> = plugin::all(ctx).into_iter().map(|p| (p.service.name.clone(), plugin::version(&plugin::text(ctx, &p.service.name)))).collect();
    let cols = collections();
    let in_col = |n: &str| cols.iter().find(|c| c["name"] == collection).map(|c| c["plugins"].as_array().map(|a| a.iter().any(|x| x == n)).unwrap_or(false)).unwrap_or(true);
    let q = query.to_lowercase();
    let all: Vec<(Plugin, String)> = files().into_iter().filter_map(|(_, t)| plugin::parse(&t).ok().map(|p| (p, t))).collect();
    let mut cats = vec![json!({"name": "All", "count": all.len()}), json!({"name": "HTTP APIs", "count": all.len()})];
    let pages = all.iter().filter(|(p, _)| !p.pages.is_empty()).count();
    if pages > 0 {
        cats.push(json!({"name": "Browser pages", "count": pages}));
    }
    let plugins: Vec<Value> = all.iter().filter(|(p, _)| {
        (q.is_empty() || format!("{} {} {} {}", p.service.name, p.service.title, p.service.description, p.service.real_host).to_lowercase().contains(&q))
            && (region.is_empty() || region == "Any" || (region == "Global" && p.service.region.is_empty()) || p.service.region.eq_ignore_ascii_case(region) || (region == "Brazil" && p.service.region == "BR"))
            && (category != "Browser pages" || !p.pages.is_empty())
            && (collection.is_empty() || in_col(&p.service.name))
    }).map(|(p, t)| card(p, t, &installed)).collect();
    let collections: Vec<Value> = cols.iter().map(|c| {
        let names: Vec<String> = c["plugins"].as_array().map(|a| a.iter().map(expr::text).collect()).unwrap_or_default();
        let members: Vec<Value> = names.iter().map(|n| {
            let p = all.iter().find(|(p, _)| &p.service.name == n);
            json!({"name": n, "title": p.map(|(p, _)| p.service.title.clone()).unwrap_or(n.clone()), "installed": installed.contains_key(n), "exists": p.is_some()})
        }).collect();
        json!({"name": c["name"], "description": c["description"], "plugins": members, "installed": members.iter().filter(|m| m["installed"] == true).count(), "total": members.len()})
    }).collect();
    json!({"plugins": plugins, "categories": cats, "collections": collections, "source": std::fs::canonicalize(plugin::catalog_dir()).unwrap_or(plugin::catalog_dir()).display().to_string()})
}

/// K2 (and K3): what a plugin answers, the pages it draws, its people, what you can provoke.
pub fn detail(p: &Plugin, t: &str, files: Vec<Value>) -> Value {
    let people: Vec<Value> = p.data.get("people").and_then(|x| x.as_array()).cloned().unwrap_or_default().iter().map(|r| json!({
        "name": r["name"], "cpf": expr::mask(&expr::text(&r["cpf"])), "certificate": r.get("certificate").cloned().unwrap_or(json!(null)), "kind": r.get("kind").cloned().unwrap_or(json!(null)),
    })).collect();
    let routes: Vec<Value> = p.pages.iter().map(|pg| json!({"route": pg.route, "what": format!("the page “{}”{}", pg.title, if pg.push.is_some() { ", or a push approval" } else { "" })}))
        .chain(p.routes.iter().map(|r| json!({"route": r.route, "what": match r.grant.as_str() {
            "authorization_code" => "code + code_verifier → token for the person".to_string(),
            "password" => "CPF + OTP → token for the person".to_string(),
            _ if r.token => "client credentials → token".to_string(),
            _ if !r.step.is_empty() => "the app polls a push approval".to_string(),
            _ => r.decide.iter().map(|d| d.answer.clone()).collect::<Vec<_>>().join(" · "),
        }}))).collect();
    let pages: Vec<Value> = p.pages.iter().enumerate().map(|(i, pg)| json!({"index": i, "route": pg.route, "title": pg.title, "push": pg.push.is_some(),
        "fields": pg.fields.iter().map(|f| match f { Field::Person(_) => "person", Field::Otp(_) => "otp", Field::Text(_) => "text", Field::Choice(_) => "choice" }).collect::<Vec<_>>(),
        "buttons": pg.buttons, "yaml": serde_yaml::to_string(&vec![pg]).unwrap_or_default()})).collect();
    let situations: Vec<Value> = p.answers.iter().filter(|a| a.status >= 400 || !a.note.is_empty()).map(|a| json!({"name": a.name, "status": a.status, "note": a.note}))
        .chain(["429", "503", "timeout"].iter().map(|g| json!({"name": g, "status": g.parse::<u16>().unwrap_or(0), "note": "generic, any service"}))).collect();
    let sends = p.routes.iter().any(|r| !r.send.is_empty());
    json!({"name": p.service.name, "title": p.service.title, "origin": p.service.origin, "region": p.service.region, "description": p.service.description,
        "version": plugin::version(t), "lines": t.lines().count(), "routes": routes, "pages": pages, "people": people, "situations": situations, "badges": badges(p),
        "permissions": {"network": if sends { "sends webhooks to hosts listed in the environment" } else { "none" }, "disk": "its own state only", "scripts": "none: pages are fields"},
        "files": files})
}

// ---- imports ----

struct Import {
    name: String,
    yaml: String,
    source: String,
    at: i64,
}

fn imports() -> &'static Mutex<HashMap<String, Import>> {
    static I: OnceLock<Mutex<HashMap<String, Import>>> = OnceLock::new();
    I.get_or_init(Default::default)
}

fn tmp() -> PathBuf {
    let d = std::env::temp_dir().join(format!("stuntman-import-{}", util::id("imp")));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Where the plugin files are: a folder as is, a .zip unpacked, a git repository cloned (depth 1).
fn fetch(source: &str, sub: &str, git_ref: &str) -> Result<(PathBuf, String), ApiError> {
    let s = source.trim();
    let is_git = s.starts_with("http://") || s.starts_with("https://") || s.starts_with("git@") || s.ends_with(".git");
    let root = if is_git {
        let d = tmp();
        let mut c = Command::new("git");
        c.args(["clone", "--quiet", "--depth", "1"]);
        if !git_ref.is_empty() {
            c.args(["--branch", git_ref]);
        }
        let out = c.arg(s).arg(&d).env("GIT_TERMINAL_PROMPT", "0").output().map_err(|e| ApiError::internal(e.to_string()))?;
        if !out.status.success() {
            return Err(ApiError::new(502, format!("Could not read {s}: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("git failed").trim())));
        }
        let head = Command::new("git").arg("-C").arg(&d).args(["rev-parse", "--short", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        return Ok((d.join(sub.trim_matches('/')), format!("{s}{}{} @ {head}", if sub.is_empty() { "" } else { "#" }, sub)));
    } else if s.ends_with(".zip") {
        let d = tmp();
        let out = Command::new("unzip").args(["-q", "-o"]).arg(s).arg("-d").arg(&d).output().map_err(|e| ApiError::internal(e.to_string()))?;
        if !out.status.success() {
            return Err(ApiError::bad(format!("Could not unpack {s}")));
        }
        d
    } else {
        PathBuf::from(s)
    };
    Ok((root.join(sub.trim_matches('/')), s.to_string()))
}

fn plugin_file(dir: &Path) -> Option<PathBuf> {
    if dir.is_file() {
        return Some(dir.to_path_buf());
    }
    let p = dir.join("plugin.yaml");
    if p.exists() {
        return Some(p);
    }
    // A zip with one folder inside, or a folder with a single plugin file.
    let entries: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).collect();
    if let [only] = entries.iter().filter(|e| e.is_dir() && !e.file_name().map(|n| n.to_string_lossy().starts_with('.') || n == "__MACOSX").unwrap_or(false)).collect::<Vec<_>>().as_slice() {
        if entries.iter().all(|e| e.is_dir()) {
            return plugin_file(only);
        }
    }
    let yamls: Vec<&PathBuf> = entries.iter().filter(|e| e.extension().map(|x| x == "yaml" || x == "yml").unwrap_or(false) && e.file_stem().map(|n| n != "people").unwrap_or(true)).collect();
    if yamls.len() == 1 { Some(yamls[0].clone()) } else { None }
}

/// K3: reads the plugin, checks it, keeps it for install_import (1 hour).
pub fn import(source: &str, sub: &str, git_ref: &str) -> Result<Value, ApiError> {
    if source.trim().is_empty() {
        return Err(ApiError::bad("source: a folder, a .zip or a git URL"));
    }
    let (dir, from) = fetch(source, sub, git_ref)?;
    let f = plugin_file(&dir).ok_or_else(|| ApiError::bad(format!("No plugin.yaml in {}", if sub.is_empty() { source } else { sub })))?;
    let base = if f.parent().map(|p| p == dir || dir.is_file()).unwrap_or(true) { f.parent().map(Path::to_path_buf).unwrap_or(dir.clone()) } else { f.parent().unwrap().to_path_buf() };
    let mut yaml = std::fs::read_to_string(&f).map_err(|e| ApiError::bad(e.to_string()))?;
    let mut files = vec![json!({"name": f.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), "what": format!("{} lines", yaml.lines().count())})];
    let mut checks = vec![];
    // people.yaml beside it joins data.people.
    let people = base.join("people.yaml");
    if people.exists() && f != people {
        let rows: Value = serde_yaml::from_str(&std::fs::read_to_string(&people).unwrap_or_default()).unwrap_or(Value::Null);
        let rows = rows.get("people").cloned().unwrap_or(rows);
        if let (Some(arr), Ok(mut y)) = (rows.as_array(), serde_yaml::from_str::<serde_yaml::Value>(&yaml)) {
            files.push(json!({"name": "people.yaml", "what": format!("{} people", arr.len())}));
            y["data"]["people"] = serde_yaml::to_value(arr).unwrap_or_default();
            yaml = serde_yaml::to_string(&y).unwrap_or(yaml);
        }
    }
    for extra in ["README.md", "logo.svg"] {
        if let Ok(m) = std::fs::metadata(base.join(extra)) {
            files.push(json!({"name": extra, "what": if extra == "logo.svg" { format!("{} KB, shown in the catalog", (m.len() + 1023) / 1024) } else { "what it covers".into() }}));
        }
    }
    let parsed = plugin::parse(&yaml);
    let mut ok_to_install = true;
    let mut detail_v = Value::Null;
    let mut name = String::new();
    match &parsed {
        Err(e) => {
            ok_to_install = false;
            let msg = e.replace("unknown variant", "unknown field").replace("expected one of `person`, `otp`, `text`, `choice`", "Stuntman draws person, otp, text and choice");
            checks.push(json!({"ok": false, "text": format!("plugin.yaml: {msg}"), "detail": "Install stays off until the file is valid."}));
        }
        Ok(p) => {
            name = p.service.name.clone();
            let errs = plugin::validate(p, &yaml);
            if errs.is_empty() {
                checks.push(json!({"ok": true, "text": "plugin.yaml is valid", "detail": format!("{} lines, {} routes, {} answers", yaml.lines().count(), p.routes.len(), p.answers.len())}));
            } else {
                ok_to_install = false;
                for e in errs {
                    checks.push(json!({"ok": false, "text": format!("plugin.yaml{}: {}", e["line"].as_u64().map(|l| format!(", line {l}")).unwrap_or_default(), e["error"].as_str().unwrap_or("")), "detail": "Install stays off until the file is valid."}));
                }
            }
            if !p.pages.is_empty() {
                checks.push(json!({"ok": true, "text": format!("{} page(s), only known fields", p.pages.len()), "detail": "person, otp, text, choice and buttons; no HTML, no scripts"}));
            }
            if let Some(n) = p.data.get("people").and_then(|x| x.as_array()).map(|a| a.len()) {
                checks.push(json!({"ok": true, "text": format!("{n} synthetic people"), "detail": "certificates issued by the Test CA when first used"}));
            }
            let sends = p.routes.iter().any(|r| !r.send.is_empty());
            checks.push(json!({"ok": !sends, "warn": sends, "text": if sends { "Sends webhooks to your app" } else { "No network, no scripts" },
                "detail": if sends { "only to hosts listed in the environment" } else { "answers come from the file and its own state" }}));
            detail_v = detail(p, &yaml, files.clone());
        }
    }
    let id = util::id("imp");
    imports().lock().unwrap().retain(|_, i| i.at > util::now_ms() - 3_600_000);
    if ok_to_install {
        imports().lock().unwrap().insert(id.clone(), Import { name: name.clone(), yaml: yaml.clone(), source: from.clone(), at: util::now_ms() });
    }
    Ok(json!({"id": if ok_to_install { json!(id) } else { Value::Null }, "name": name, "source": from, "checks": checks, "files": files, "plugin": detail_v, "can_install": ok_to_install,
        "from_catalog": files_contains(&yaml)}))
}

fn files_contains(yaml: &str) -> bool {
    files().iter().any(|(_, t)| t == yaml)
}

/// Installs what import_plugin read, under its name or `service`.
pub fn install_import(ctx: &Ctx, id: &str, service: Option<String>) -> Result<String, ApiError> {
    let (name, yaml, source) = {
        let m = imports().lock().unwrap();
        let i = m.get(id).ok_or_else(|| ApiError::not_found("This import is gone (1 hour) or had errors; read it again"))?;
        (i.name.clone(), i.yaml.clone(), i.source.clone())
    };
    let svc = service.filter(|s| !s.is_empty()).unwrap_or(name.clone());
    plugin::valid_name(&svc)?;
    if plugin::file(ctx, &svc).exists() {
        return Err(ApiError::new(409, format!("{svc} already exists here. Install it under another name, e.g. {svc}-2.")));
    }
    let mut y: serde_yaml::Value = serde_yaml::from_str(&yaml).map_err(|e| ApiError::bad(e.to_string()))?;
    y["service"]["name"] = svc.clone().into();
    y["service"]["origin"] = "yours".into();
    let t = format!("# Imported from {source}\n{}", serde_yaml::to_string(&y).unwrap_or_default());
    plugin::save_text(ctx, &svc, &t)?;
    imports().lock().unwrap().remove(id);
    Ok(svc)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn imports_a_folder_and_refuses_unknown_fields() {
        let d = std::env::temp_dir().join(format!("stuntman-imp-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        std::fs::write(d.join("plugin.yaml"), "service: {name: psc-generic}\npages:\n  - route: GET /authorize\n    title: Any PSC\n    fields:\n      - person: {from: people}\nroutes:\n  - route: POST /token\n    grant: authorization_code\n").unwrap();
        std::fs::write(d.join("people.yaml"), "- {name: Olivia Brooks, cpf: '321.654.987-91'}\n").unwrap();
        let r = import(d.to_str().unwrap(), "", "").unwrap();
        assert_eq!(r["can_install"], true, "{r}");
        assert_eq!(r["files"][1]["name"], "people.yaml");
        std::fs::write(d.join("plugin.yaml"), "service: {name: x}\npages:\n  - route: GET /a\n    title: A\n    fields:\n      - captcha: {}\n").unwrap();
        let r = import(d.to_str().unwrap(), "", "").unwrap();
        assert_eq!(r["can_install"], false);
        assert!(r["checks"][0]["text"].as_str().unwrap().contains("captcha"), "{r}");
    }

    #[test]
    fn catalog_plugins_are_valid() {
        for (n, t) in files() {
            let p = plugin::parse(&t).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert!(plugin::validate(&p, &t).is_empty(), "{n}: {:?}", plugin::validate(&p, &t));
        }
    }
}
