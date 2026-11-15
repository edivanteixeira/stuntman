//! A18: test certificates in the ICP-Brasil style (e-CNPJ, e-CPF) for apps that sign fiscal
//! documents or call SEFAZ-like services with mTLS. Synthetic holders only; valid only where the
//! Stuntman Test CA is trusted. Also signs and checks XML with them.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx, Tool};

pub const SQL: &str = "
CREATE TABLE IF NOT EXISTS api_certs(id TEXT PRIMARY KEY, at INTEGER NOT NULL, holder TEXT NOT NULL, cnpj TEXT NOT NULL DEFAULT '', cpf TEXT NOT NULL DEFAULT '',
  not_before INTEGER NOT NULL, not_after INTEGER NOT NULL, untrusted INTEGER NOT NULL DEFAULT 0, cert TEXT NOT NULL, key TEXT NOT NULL);
";

fn view(r: &rusqlite::Row) -> rusqlite::Result<Value> {
    let (nb, na): (i64, i64) = (r.get(5)?, r.get(6)?);
    let now = util::now_ms() / 1000;
    let untrusted: bool = r.get(7)?;
    let state = if untrusted { "untrusted CA" } else if na < now { "expired" } else if nb > now { "not yet valid" } else { "valid" };
    Ok(json!({"id": r.get::<_, String>(0)?, "at": r.get::<_, i64>(1)?, "holder": r.get::<_, String>(2)?, "cnpj": r.get::<_, String>(3)?, "cpf": r.get::<_, String>(4)?,
        "notBefore": nb * 1000, "notAfter": na * 1000, "state": state, "type": if r.get::<_, String>(3)?.is_empty() { "A1 · e-CPF" } else { "A1 · e-CNPJ" }}))
}

const COLS: &str = "id, at, holder, cnpj, cpf, not_before, not_after, untrusted";

async fn key_of(ctx: &Ctx, id: &str) -> Result<(String, Vec<u8>), ApiError> {
    let id = id.to_string();
    let r: Option<(String, String)> = ctx.db.call(move |cn| cn.query_row("SELECT key, cert FROM api_certs WHERE id=?1 OR holder=?1 ORDER BY at DESC LIMIT 1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()).await?;
    let (k, c) = r.ok_or_else(|| ApiError::not_found("No such test certificate (new_test_certificate)"))?;
    Ok((k, B64.decode(c).unwrap_or_default()))
}

fn synthetic(holder: &str) -> Result<(), ApiError> {
    let h = holder.to_lowercase();
    if ["receita", "sefaz", "petrobras", "banco do brasil", "caixa econ", "itau", "bradesco", "santander"].iter().any(|x| h.contains(x)) {
        return Err(ApiError::bad("Synthetic holders only (Acme Transportes Ltda, Globex Logistica…)"));
    }
    Ok(())
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("new_test_certificate", "An ICP-Brasil style A1 test certificate: an e-CNPJ (cnpj) or e-CPF (cpf) with an RSA 2048 key, signed by the Stuntman Test CA (or an unknown CA with untrusted=true), valid for valid_days or already expired. Returns the .pfx (base64) and its password, the certificate and key PEM. For mTLS to services with client_certificate_icp and for signing XML.", |ctx, a| async move {
            let holder = a.need("holder")?;
            synthetic(&holder)?;
            let cnpj = a.str("cnpj");
            let cpf = a.str("cpf");
            if cnpj.is_none() && cpf.is_none() {
                return Err(ApiError::bad("cnpj or cpf"));
            }
            let now = util::now_ms() / 1000;
            let (nb, na) = if a.bool("expired").unwrap_or(false) { (now - 400 * 86_400, now - 86_400) } else if a.bool("not_yet_valid").unwrap_or(false) { (now + 86_400, now + 366 * 86_400) } else { (now - 3600, now + a.u64("valid_days").unwrap_or(365) as i64 * 86_400) };
            let untrusted = a.bool("untrusted").unwrap_or(false);
            let (key, der) = crate::ca::get().icp(&holder, cnpj.as_deref(), cpf.as_deref(), nb, na, untrusted).map_err(ApiError::internal)?;
            let password = a.str("password").unwrap_or_else(|| util::uuid().replace('-', "")[..12].to_string());
            let pfx = crate::ca::get().pfx(&key, &der, &password).map_err(ApiError::internal)?;
            let id = util::id("cert");
            let digits = |s: &Option<String>| s.as_ref().map(|x| x.chars().filter(|c| c.is_ascii_digit()).collect::<String>()).unwrap_or_default();
            let (i, h, cn, cp, c, k) = (id.clone(), holder.clone(), digits(&cnpj), digits(&cpf), B64.encode(&der), key.clone());
            ctx.db.call(move |db| db.execute("INSERT INTO api_certs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)", params![i, util::now_ms(), h, cn, cp, nb, na, untrusted, c, k]).map(|_| ())).await?;
            ctx.live.send("apis.certs", &id, "created");
            let pem = format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", B64.encode(&der).as_bytes().chunks(64).map(|c| String::from_utf8_lossy(c).to_string()).collect::<Vec<_>>().join("\n"));
            Ok(json!({"id": id, "holder": holder, "pfx": B64.encode(pfx), "password": password, "certificate": pem, "key": key, "cnpj": crate::ca::cnpj_of(&der),
                "notBefore": nb * 1000, "notAfter": na * 1000, "untrusted": untrusted}))
        })
        .req("holder", "a synthetic company or person, e.g. Acme Transportes Ltda").param("cnpj", "e-CNPJ: 11.222.333/0001-81").param("cpf", "e-CPF").param("valid_days", "365")
        .param("expired", "bool: already expired").param("not_yet_valid", "bool").param("untrusted", "bool: signed by an unknown CA").param("password", "the .pfx password (default: random, shown once)")
        .route("POST", "/apis/test-certificates"),
        Tool::new("list_test_certificates", "The test certificates made in this environment (holder, CNPJ or CPF, validity, state). Keys are not shown.", |ctx, _a| async move {
            let rows = ctx.db.call(|cn| {
                let mut st = cn.prepare(&format!("SELECT {COLS} FROM api_certs ORDER BY at DESC LIMIT 200"))?;
                let v: Vec<Value> = st.query_map([], view)?.filter_map(|x| x.ok()).collect();
                Ok(v)
            }).await?;
            Ok(json!({"certificates": rows}))
        })
        .route("GET", "/apis/test-certificates"),
        Tool::new("sign_xml", "Signs an XML document's element (by Id) with a test certificate, as an app would sign an NF-e or CT-e: enveloped ds:Signature after the element (or inside it for SAML-like documents), RSA-SHA1 by default.", |ctx, a| async move {
            let (key, der) = key_of(&ctx, &a.need("certificate")?).await?;
            let xml = a.need("xml")?;
            let id = a.need("id")?;
            let inside = a.str("place").map(|p| p == "inside").unwrap_or(false);
            let out = crate::xmldsig::sign(&xml, &id, &key, &der, !a.bool("sha256").unwrap_or(false), a.bool("exclusive").unwrap_or(false), if inside { crate::xmldsig::Place::InsideLast } else { crate::xmldsig::Place::AfterElement }, if a.bool("prefix").unwrap_or(false) { "ds" } else { "" }).map_err(ApiError::bad)?;
            Ok(json!({"xml": out}))
        })
        .req("xml", "the document").req("id", "the Id of the element to sign").req("certificate", "a test certificate's id or holder").param("sha256", "bool").param("exclusive", "bool: exclusive c14n").param("place", "after | inside").param("prefix", "bool: ds: prefix")
        .route("POST", "/apis/xml/sign"),
        Tool::new("verify_xml_signature", "Checks every ds:Signature of an XML document: digests and signature value, with the certificate in KeyInfo. Says what was signed and by which CNPJ.", |_ctx, a| async move {
            match crate::xmldsig::verify_embedded(&a.need("xml")?) {
                Ok(c) => Ok(json!({"valid": true, "signatures": c.iter().map(|x| json!({"reference": x.reference, "algorithm": x.algorithm, "cnpj": crate::ca::cnpj_of(&x.certificate)})).collect::<Vec<_>>()})),
                Err(e) => Ok(json!({"valid": false, "error": e})),
            }
        })
        .req("xml", "the document")
        .route("POST", "/apis/xml/verify"),
    ]
}
