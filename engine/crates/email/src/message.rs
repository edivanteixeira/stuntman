//! Messages, their attachments and their events (E1).

use stuntman_core::{util, ApiError, Ctx};
use rusqlite::{params, params_from_iter, OptionalExtension, Row};
use serde::Serialize;
use serde_json::{json, Value};


#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub index: u32,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    pub sha256: String,
    pub inline: bool,
    #[serde(skip)]
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub seq: i64,
    pub service: String,
    /// smtp | sendgrid
    pub via: String,
    pub at: i64,
    pub from: String,
    pub from_name: String,
    pub to: String,
    pub subject: String,
    pub kind: String,
    pub lang: String,
    pub status: String,
    /// you | step | mcp:<client> | api:<client> | rule
    pub status_by: String,
    pub status_at: Option<i64>,
    /// kept | temporary | catch-all
    pub inbox: String,
    pub trace_id: Option<String>,
    /// Set by an inbox rule or by hand (bounce, drop, defer): the ◆ on the screen.
    pub provoked: bool,
    pub extracted: Value,
    pub links: Value,
    pub sg_message_id: String,
    pub held_until: Option<i64>,
    pub reason: Option<String>,
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<Attachment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<Vec<Value>>,
    pub attachment_count: u32,
    #[serde(skip)]
    pub custom_args: Value,
    #[serde(skip)]
    pub raw: Vec<u8>,
}

const LIST_COLS: &str = "id, seq, service, via, at, from_addr, from_name, rcpt, subject, kind, lang, status, status_by, status_at, inbox_kind, trace_id,
  provoked, extracted, links, sg_id, held_until, reason, size, (SELECT COUNT(*) FROM email_attachments a WHERE a.message_id = email_messages.id AND a.inline = 0), custom_args";

fn row(r: &Row) -> rusqlite::Result<Message> {
    let j = |i: usize| -> rusqlite::Result<Value> { Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or(Value::Null)) };
    Ok(Message {
        id: r.get(0)?,
        seq: r.get(1)?,
        service: r.get(2)?,
        via: r.get(3)?,
        at: r.get(4)?,
        from: r.get(5)?,
        from_name: r.get(6)?,
        to: r.get(7)?,
        subject: r.get(8)?,
        kind: r.get(9)?,
        lang: r.get(10)?,
        status: r.get(11)?,
        status_by: r.get(12)?,
        status_at: r.get(13)?,
        inbox: r.get(14)?,
        trace_id: r.get(15)?,
        provoked: r.get(16)?,
        extracted: j(17)?,
        links: j(18)?,
        sg_message_id: r.get(19)?,
        held_until: r.get(20)?,
        reason: r.get(21)?,
        size: r.get(22)?,
        attachment_count: r.get(23)?,
        custom_args: j(24)?,
        ..Default::default()
    })
}

/// Stores one message per recipient; returns it with its id.
pub async fn insert(ctx: &Ctx, m: Message, text: String, html: String, headers: Value, atts: Vec<Attachment>) -> Result<Message, ApiError> {
    ctx.db
        .call(move |c| {
            let tx = c.transaction()?;
            tx.execute(
                "INSERT INTO email_messages(service, via, at, from_addr, from_name, rcpt, subject, kind, lang, text, html, headers, raw, size, status, status_by,
                   status_at, inbox_kind, trace_id, provoked, extracted, links, sg_id, custom_args, held_until, reason)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26)",
                params![
                    m.service, m.via, m.at, m.from, m.from_name, m.to, m.subject, m.kind, m.lang, text, html, headers.to_string(), m.raw, m.size,
                    m.status, m.status_by, m.status_at, m.inbox, m.trace_id, m.provoked, m.extracted.to_string(), m.links.to_string(),
                    m.sg_message_id, m.custom_args.to_string(), m.held_until, m.reason
                ],
            )?;
            let seq = tx.last_insert_rowid();
            let id = format!("msg_{seq:04}");
            tx.execute("UPDATE email_messages SET id = ?1 WHERE seq = ?2", params![id, seq])?;
            for a in &atts {
                tx.execute(
                    "INSERT INTO email_attachments(message_id, idx, name, ctype, size, sha256, inline, data) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![id, a.index, a.name, a.content_type, a.size, a.sha256, a.inline, a.data],
                )?;
            }
            tx.commit()?;
            Ok(Message { id, seq, attachment_count: atts.iter().filter(|a| !a.inline).count() as u32, ..m })
        })
        .await
}

#[derive(Default, Debug, Clone)]
pub struct Filter {
    pub to: Option<String>,
    pub subject: Option<String>,
    pub kind: Option<String>,
    pub inbox: Vec<String>,
    pub status: Vec<String>,
    pub lang: Vec<String>,
    pub has: Vec<String>,
    pub since_ms: Option<i64>,
    pub before_ms: Option<i64>,
    pub q: Option<String>,
    pub service: Option<String>,
    pub limit: u32,
    pub after_seq: Option<i64>,
}

fn where_sql(f: &Filter) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value as V;
    let mut w = vec!["1=1".to_string()];
    let mut p: Vec<V> = vec![];
    fn push(w: &mut Vec<String>, p: &mut Vec<rusqlite::types::Value>, cond: &str, v: rusqlite::types::Value) {
        p.push(v);
        w.push(cond.replace('?', &format!("?{}", p.len())));
    }
    if let Some(t) = &f.to {
        push(&mut w, &mut p, "rcpt = ? COLLATE NOCASE", V::Text(t.clone()));
    }
    if let Some(s) = &f.subject {
        push(&mut w, &mut p, "subject LIKE '%' || ? || '%'", V::Text(s.clone()));
    }
    if let Some(k) = &f.kind {
        push(&mut w, &mut p, "kind = ?", V::Text(k.clone()));
    }
    if let Some(s) = &f.service {
        push(&mut w, &mut p, "service = ?", V::Text(s.clone()));
    }
    if let Some(t) = f.since_ms {
        push(&mut w, &mut p, "at >= ?", V::Integer(t));
    }
    if let Some(t) = f.before_ms {
        push(&mut w, &mut p, "at < ?", V::Integer(t));
    }
    if let Some(t) = f.after_seq {
        push(&mut w, &mut p, "seq > ?", V::Integer(t));
    }
    if let Some(q) = &f.q {
        push(&mut w, &mut p, "(rcpt LIKE '%' || ? || '%' OR subject LIKE '%' || ? || '%' OR extracted LIKE '%' || ? || '%' OR from_addr LIKE '%' || ? || '%')", V::Text(q.clone()));
    }
    for (col, vals) in [("status", &f.status), ("lang", &f.lang), ("inbox_kind", &f.inbox)] {
        if !vals.is_empty() {
            let mut ors = vec![];
            for v in vals.iter() {
                p.push(V::Text(v.clone()));
                ors.push(format!("{col} = ?{}", p.len()));
            }
            w.push(format!("({})", ors.join(" OR ")));
        }
    }
    for h in &f.has {
        p.push(V::Text(format!("$.\"{}\"", h.replace('"', ""))));
        w.push(format!("json_extract(extracted, ?{}) IS NOT NULL", p.len()));
    }
    (w.join(" AND "), p)
}

pub async fn list(ctx: &Ctx, f: Filter) -> Result<(Vec<Message>, u64), ApiError> {
    ctx.db
        .call(move |c| {
            let (w, p) = where_sql(&f);
            let total: u64 = c.query_row(&format!("SELECT COUNT(*) FROM email_messages WHERE {w}"), params_from_iter(p.iter()), |r| r.get(0))?;
            let mut st = c.prepare(&format!("SELECT {LIST_COLS} FROM email_messages WHERE {w} ORDER BY seq DESC LIMIT {}", f.limit.clamp(1, 1000)))?;
            let v = st.query_map(params_from_iter(p.iter()), row)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok((v, total))
        })
        .await
}

/// E1d: messages grouped by kind, with counts.
pub async fn kinds(ctx: &Ctx, f: Filter) -> Result<Vec<Value>, ApiError> {
    ctx.db
        .call(move |c| {
            let (w, p) = where_sql(&f);
            let mut st = c.prepare(&format!("SELECT kind, COUNT(*), MAX(at) FROM email_messages WHERE {w} GROUP BY kind ORDER BY MAX(seq) DESC"))?;
            let v = st
                .query_map(params_from_iter(p.iter()), |r| Ok(json!({"kind": r.get::<_, String>(0)?, "count": r.get::<_, u64>(1)?, "last": r.get::<_, i64>(2)?})))?
                .collect::<rusqlite::Result<Vec<_>>>();
            v
        })
        .await
}

pub async fn get(ctx: &Ctx, id: &str, full: bool) -> Result<Message, ApiError> {
    let id2 = id.to_string();
    let m = ctx
        .db
        .call(move |c| {
            let Some(mut m) = c.query_row(&format!("SELECT {LIST_COLS} FROM email_messages WHERE id = ?1"), [&id2], row).optional()? else {
                return Ok(None);
            };
            if full {
                let (text, html, headers): (String, String, String) =
                    c.query_row("SELECT text, html, headers FROM email_messages WHERE id = ?1", [&id2], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                m.text = Some(text);
                m.html = Some(html);
                m.headers = serde_json::from_str(&headers).ok();
                let mut st = c.prepare("SELECT idx, name, ctype, size, sha256, inline FROM email_attachments WHERE message_id = ?1 ORDER BY idx")?;
                m.attachments = Some(
                    st.query_map([&id2], |r| {
                        Ok(Attachment { index: r.get(0)?, name: r.get(1)?, content_type: r.get(2)?, size: r.get(3)?, sha256: r.get(4)?, inline: r.get(5)?, data: vec![] })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
                );
                let mut st = c.prepare("SELECT at, type, by_who, detail, reply FROM email_events WHERE message_id = ?1 ORDER BY id")?;
                m.events = Some(
                    st.query_map([&id2], |r| {
                        let reply: Option<String> = r.get(4)?;
                        Ok(json!({"at": r.get::<_, i64>(0)?, "type": r.get::<_, String>(1)?, "by": r.get::<_, String>(2)?,
                                  "detail": r.get::<_, String>(3)?, "reply": reply.and_then(|s| serde_json::from_str::<Value>(&s).ok())}))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?,
                );
            }
            Ok(Some(m))
        })
        .await?;
    m.ok_or_else(|| ApiError::not_found(format!("No message {id}")))
}

pub async fn raw(ctx: &Ctx, id: &str) -> Result<Vec<u8>, ApiError> {
    let id2 = id.to_string();
    ctx.db
        .call(move |c| c.query_row("SELECT raw FROM email_messages WHERE id = ?1", [id2], |r| r.get::<_, Option<Vec<u8>>>(0)).optional())
        .await?
        .flatten()
        .ok_or_else(|| ApiError::not_found(format!("No message {id}")))
}

pub async fn attachment(ctx: &Ctx, id: &str, which: &str) -> Result<Attachment, ApiError> {
    let (id2, w) = (id.to_string(), which.to_string());
    ctx.db
        .call(move |c| {
            c.query_row(
                "SELECT idx, name, ctype, size, sha256, inline, data FROM email_attachments WHERE message_id = ?1 AND (CAST(idx AS TEXT) = ?2 OR name = ?2)",
                [id2, w],
                |r| Ok(Attachment { index: r.get(0)?, name: r.get(1)?, content_type: r.get(2)?, size: r.get(3)?, sha256: r.get(4)?, inline: r.get(5)?, data: r.get(6)? }),
            )
            .optional()
        })
        .await?
        .ok_or_else(|| ApiError::not_found(format!("No attachment “{which}” in {id}")))
}

pub async fn set_status(ctx: &Ctx, id: &str, status: &str, by: &str, provoked: Option<bool>, reason: Option<String>) -> Result<(), ApiError> {
    let (id2, st, by) = (id.to_string(), status.to_string(), by.to_string());
    ctx.db
        .call(move |c| {
            c.execute(
                "UPDATE email_messages SET status = ?2, status_by = ?3, status_at = ?4, provoked = COALESCE(?5, provoked), reason = COALESCE(?6, reason),
                   held_until = CASE WHEN ?2 = 'held' THEN held_until ELSE NULL END WHERE id = ?1",
                params![id2, st, by, util::now_ms(), provoked, reason],
            )
        })
        .await?;
    ctx.live.send("email.message", id, "updated");
    stuntman_core::trips::set_status(ctx, id, status).await
}

pub async fn add_event(ctx: &Ctx, id: &str, kind: &str, by: &str, detail: &str, reply: Option<Value>) -> Result<(), ApiError> {
    let (id2, k, b, d) = (id.to_string(), kind.to_string(), by.to_string(), detail.to_string());
    ctx.db
        .call(move |c| {
            c.execute(
                "INSERT INTO email_events(message_id, at, type, by_who, detail, reply) VALUES(?1,?2,?3,?4,?5,?6)",
                params![id2, util::now_ms(), k, b, d, reply.map(|r| r.to_string())],
            )
        })
        .await?;
    ctx.live.send("email.message", id, "updated");
    Ok(())
}

pub async fn delete(ctx: &Ctx, f: Filter, id: Option<String>) -> Result<u64, ApiError> {
    let n = ctx
        .db
        .call(move |c| {
            if let Some(id) = id {
                return Ok(c.execute("DELETE FROM email_messages WHERE id = ?1", [id])? as u64);
            }
            let (w, p) = where_sql(&f);
            Ok(c.execute(&format!("DELETE FROM email_messages WHERE {w}"), params_from_iter(p.iter()))? as u64)
        })
        .await?;
    ctx.live.send("email.message", "*", "deleted");
    Ok(n)
}

/// Extractor match counts for "matched 41 today" and "no match in 2 days".
pub async fn hit(ctx: &Ctx, names: Vec<String>) -> Result<(), ApiError> {
    if names.is_empty() {
        return Ok(());
    }
    ctx.db
        .call(move |c| {
            let now = util::now_ms();
            for n in names {
                c.execute("INSERT INTO email_extractor_hits(name, at) VALUES(?1, ?2)", params![n, now])?;
            }
            Ok(())
        })
        .await
}

pub async fn hit_stats(ctx: &Ctx) -> Result<std::collections::HashMap<String, (u64, Option<i64>)>, ApiError> {
    ctx.db
        .call(|c| {
            let day = util::now_ms() - 86_400_000;
            let mut st = c.prepare("SELECT name, SUM(at >= ?1), MAX(at) FROM email_extractor_hits GROUP BY name")?;
            let v = st
                .query_map([day], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, u64>(1)?, r.get::<_, Option<i64>>(2)?))))?
                .collect::<rusqlite::Result<std::collections::HashMap<_, _>>>();
            v
        })
        .await
}

/// Retention: drop messages older than `keep_ms`, keep at most `max`.
pub async fn sweep(ctx: &Ctx, keep_ms: u64, max: u64) -> Result<(), ApiError> {
    ctx.db
        .call(move |c| {
            let cut = util::now_ms() - keep_ms as i64;
            c.execute("DELETE FROM email_messages WHERE at < ?1", [cut])?;
            c.execute("DELETE FROM email_messages WHERE seq <= (SELECT seq FROM email_messages ORDER BY seq DESC LIMIT 1 OFFSET ?1)", [max as i64])?;
            c.execute("DELETE FROM email_extractor_hits WHERE at < ?1", [util::now_ms() - 30 * 86_400_000])?;
            c.execute("DELETE FROM email_inboxes WHERE kind = 'temporary' AND expires_at < ?1", [cut])?;
            Ok(())
        })
        .await
}
