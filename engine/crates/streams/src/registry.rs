//! A Confluent-compatible Schema Registry (A17): subjects and versions, schema ids, lookup,
//! compatibility answers and config, the errors clients read (40401, 40402, 40403, 42201).
//! Its own port (8081); the default environment answers.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{env::Envs, util, Ctx};

fn err(st: u16, code: i64, msg: &str) -> Response {
    (StatusCode::from_u16(st).unwrap_or(StatusCode::BAD_REQUEST), [("content-type", "application/vnd.schemaregistry.v1+json")], Json(json!({"error_code": code, "message": msg}))).into_response()
}

fn ok(v: Value) -> Response {
    ([("content-type", "application/vnd.schemaregistry.v1+json")], Json(v)).into_response()
}

/// Same text, ignoring whitespace in JSON.
fn canon(s: &str) -> String {
    serde_json::from_str::<Value>(s).map(|v| v.to_string()).unwrap_or_else(|_| s.trim().to_string())
}

pub async fn register(ctx: &Ctx, subject: &str, schema: &str, ty: &str) -> Result<(i64, i64), String> {
    let (sub, sc, t) = (subject.to_string(), canon(schema), if ty.is_empty() { "AVRO".to_string() } else { ty.to_uppercase() });
    if t == "AVRO" && serde_json::from_str::<Value>(&sc).is_err() && !sc.starts_with('"') {
        return Err("Invalid schema".into());
    }
    let r = ctx.db.call(move |cn| {
        let id: i64 = match cn.query_row("SELECT id FROM kafka_schemas WHERE schema=?1 AND type=?2", params![sc, t], |r| r.get(0)).optional()? {
            Some(i) => i,
            None => { cn.execute("INSERT INTO kafka_schemas(schema, type) VALUES(?1,?2)", params![sc, t])?; cn.last_insert_rowid() }
        };
        if let Some(v) = cn.query_row("SELECT version FROM kafka_subjects WHERE subject=?1 AND schema_id=?2 AND deleted=0", params![sub, id], |r| r.get::<_, i64>(0)).optional()? {
            return Ok((id, v));
        }
        let v: i64 = cn.query_row("SELECT COALESCE(MAX(version),0)+1 FROM kafka_subjects WHERE subject=?1", [&sub], |r| r.get(0))?;
        cn.execute("INSERT INTO kafka_subjects VALUES(?1,?2,?3,0,?4)", params![sub, v, id, util::now_ms()])?;
        Ok((id, v))
    }).await.map_err(|e| e.message)?;
    ctx.live.send("streams.schemas", subject, "created");
    Ok(r)
}

pub async fn version(ctx: &Ctx, subject: &str, v: &str) -> Option<Value> {
    let (sub, v) = (subject.to_string(), v.to_string());
    ctx.db.call(move |cn| {
        let q = if v == "latest" || v == "-1" { "SELECT s.version, s.schema_id, k.schema, k.type FROM kafka_subjects s JOIN kafka_schemas k ON k.id=s.schema_id WHERE s.subject=?1 AND s.deleted=0 ORDER BY s.version DESC LIMIT 1".to_string() }
            else { "SELECT s.version, s.schema_id, k.schema, k.type FROM kafka_subjects s JOIN kafka_schemas k ON k.id=s.schema_id WHERE s.subject=?1 AND s.deleted=0 AND s.version=?2".to_string() };
        let mut st = cn.prepare(&q)?;
        let r = if v == "latest" || v == "-1" { st.query_row(params![sub], |r| Ok(json!({"subject": sub, "version": r.get::<_, i64>(0)?, "id": r.get::<_, i64>(1)?, "schema": r.get::<_, String>(2)?, "schemaType": r.get::<_, String>(3)?}))).optional()? }
            else { st.query_row(params![sub, v.parse::<i64>().unwrap_or(0)], |r| Ok(json!({"subject": sub, "version": r.get::<_, i64>(0)?, "id": r.get::<_, i64>(1)?, "schema": r.get::<_, String>(2)?, "schemaType": r.get::<_, String>(3)?}))).optional()? };
        Ok(r)
    }).await.ok().flatten()
}

pub async fn by_id(ctx: &Ctx, id: i64) -> Option<(String, String)> {
    ctx.db.call(move |cn| cn.query_row("SELECT schema, type FROM kafka_schemas WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await.ok().flatten()
}

pub async fn subjects(ctx: &Ctx) -> Vec<Value> {
    ctx.db.call(|cn| {
        let mut st = cn.prepare("SELECT subject, GROUP_CONCAT(version), MAX(schema_id) FROM kafka_subjects WHERE deleted=0 GROUP BY subject ORDER BY subject")?;
        let v = st.query_map([], |r| Ok(json!({"subject": r.get::<_, String>(0)?, "versions": r.get::<_, String>(1)?.split(',').filter_map(|x| x.parse::<i64>().ok()).collect::<Vec<_>>(), "latestId": r.get::<_, i64>(2)?})))?.filter_map(|x| x.ok()).collect();
        Ok(v)
    }).await.unwrap_or_default()
}

fn ctx(envs: &Envs) -> Option<Ctx> {
    envs.default_env()
}

pub fn router(envs: Envs) -> Router {
    Router::new()
        .route("/subjects", get(|State(e): State<Envs>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            ok(json!(subjects(&c).await.into_iter().map(|s| s["subject"].clone()).collect::<Vec<_>>()))
        }))
        .route("/subjects/{subject}/versions", get(|State(e): State<Envs>, Path(s): Path<String>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            match subjects(&c).await.into_iter().find(|x| x["subject"] == json!(s)) { Some(x) => ok(x["versions"].clone()), None => err(404, 40401, &format!("Subject '{s}' not found.")) }
        }).post(|State(e): State<Envs>, Path(s): Path<String>, Json(b): Json<Value>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            if config_of(&c).provoke == "registry_down" { return err(503, 50003, "Error while forwarding the request to the master (provoked in Stuntman)") }
            match register(&c, &s, b["schema"].as_str().unwrap_or(""), b["schemaType"].as_str().unwrap_or("")).await { Ok((id, _)) => ok(json!({"id": id})), Err(m) => err(422, 42201, &m) }
        }))
        .route("/subjects/{subject}/versions/{v}", get(|State(e): State<Envs>, Path((s, v)): Path<(String, String)>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            match version(&c, &s, &v).await { Some(x) => ok(x), None if subjects(&c).await.iter().any(|x| x["subject"] == json!(s)) => err(404, 40402, &format!("Version {v} not found.")), None => err(404, 40401, &format!("Subject '{s}' not found.")) }
        }))
        .route("/subjects/{subject}/versions/{v}/schema", get(|State(e): State<Envs>, Path((s, v)): Path<(String, String)>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            match version(&c, &s, &v).await { Some(x) => ([("content-type", "application/json")], x["schema"].as_str().unwrap_or("").to_string()).into_response(), None => err(404, 40402, "Version not found.") }
        }))
        .route("/subjects/{subject}", post(|State(e): State<Envs>, Path(s): Path<String>, Json(b): Json<Value>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            let want = canon(b["schema"].as_str().unwrap_or(""));
            let s2 = s.clone();
            let hit = c.db.call(move |cn| cn.query_row("SELECT s.version, s.schema_id, k.schema FROM kafka_subjects s JOIN kafka_schemas k ON k.id=s.schema_id WHERE s.subject=?1 AND k.schema=?2 AND s.deleted=0",
                params![s2, want], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))).optional()).await.ok().flatten();
            match hit { Some((v, id, sc)) => ok(json!({"subject": s, "version": v, "id": id, "schema": sc})), None => err(404, 40403, "Schema not found") }
        }).delete(|State(e): State<Envs>, Path(s): Path<String>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            let s2 = s.clone();
            let vs = c.db.call(move |cn| {
                let mut st = cn.prepare("SELECT version FROM kafka_subjects WHERE subject=?1 AND deleted=0")?;
                let v: Vec<i64> = st.query_map([&s2], |r| r.get(0))?.filter_map(|x| x.ok()).collect();
                cn.execute("UPDATE kafka_subjects SET deleted=1 WHERE subject=?1", [&s2])?;
                Ok(v)
            }).await.unwrap_or_default();
            if vs.is_empty() { err(404, 40401, &format!("Subject '{s}' not found.")) } else { ok(json!(vs)) }
        }))
        .route("/schemas/ids/{id}", get(|State(e): State<Envs>, Path(id): Path<i64>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            match by_id(&c, id).await { Some((s, t)) => ok(if t == "AVRO" { json!({"schema": s}) } else { json!({"schema": s, "schemaType": t}) }), None => err(404, 40403, "Schema not found") }
        }))
        .route("/schemas/ids/{id}/versions", get(|State(e): State<Envs>, Path(id): Path<i64>| async move {
            let Some(c) = ctx(&e) else { return err(500, 50001, "no environment") };
            let v = c.db.call(move |cn| {
                let mut st = cn.prepare("SELECT subject, version FROM kafka_subjects WHERE schema_id=?1 AND deleted=0")?;
                let v: Vec<Value> = st.query_map([id], |r| Ok(json!({"subject": r.get::<_, String>(0)?, "version": r.get::<_, i64>(1)?})))?.filter_map(|x| x.ok()).collect();
                Ok(v)
            }).await.unwrap_or_default();
            ok(json!(v))
        }))
        .route("/compatibility/subjects/{subject}/versions/{v}", post(|| async { ok(json!({"is_compatible": true})) }))
        .route("/config", get(|| async { ok(json!({"compatibilityLevel": "BACKWARD"})) }).put(|Json(b): Json<Value>| async move { ok(json!({"compatibility": b["compatibility"]})) }))
        .route("/config/{subject}", get(|| async { ok(json!({"compatibilityLevel": "BACKWARD"})) }).put(|Json(b): Json<Value>| async move { ok(json!({"compatibility": b["compatibility"]})) }))
        .route("/", get(|| async { ok(json!({})) }))
        .with_state(envs)
}

fn config_of(c: &Ctx) -> crate::config::Config {
    crate::config::load(c)
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("Schema Registry: port {port} is taken");
            return;
        };
        let _ = axum::serve(l, router(envs)).await;
    });
}
