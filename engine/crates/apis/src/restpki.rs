//! Lacuna REST PKI with the Web PKI extension, for a plugin whose routes say `native: restpki.*`.
//! The app starts a PAdES signature (PDF, page, position in cm, stamp PNG) and gets a token; the
//! browser hands the certificate and gets the signed attributes, signs them with its own key and
//! posts the signature; the app collects the signed PDF. The steps answer with the plugin's named
//! answers ({{ result }}), so Calls, force_answer and the timeline work as for any plugin.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Map, Value};
use sha1::Digest as _;
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};
use x509_parser::prelude::{FromDer, X509Certificate, X509Name};

use crate::{pades, plugin::Plugin, store};

/// Native steps and the answers each one may pick (checked when the plugin is saved).
pub const NATIVE: [(&str, &[&str]); 4] = [
    ("restpki.start", &["started", "validation_error"]),
    ("restpki.webpki_start", &["to_sign", "token_not_found", "validation_error"]),
    ("restpki.webpki_signature", &["signature_ok", "invalid_signature", "token_not_found", "validation_error"]),
    ("restpki.finish", &["signed", "signature_not_found", "token_not_found", "validation_error"]),
];

const SESSIONS: &str = "sessions";

pub struct Native {
    pub answer: &'static str,
    /// What the answer renders ({{ result }}).
    pub result: Value,
    /// What Calls shows as sent ({{ call }} in sentences).
    pub note: Value,
}

/// Case-insensitive lookup along a path (the .NET client sends PascalCase).
fn gi<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |d, k| d.as_object()?.iter().find(|(kk, _)| kk.eq_ignore_ascii_case(k)).map(|x| x.1)).filter(|x| !x.is_null())
}

fn num(v: &Value, path: &[&str]) -> f64 {
    gi(v, path).and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok()))).unwrap_or(0.0)
}

fn fail(answer: &'static str, code: &str, message: String, token: &str) -> Native {
    Native { answer, result: json!({"code": code, "message": message, "detail": message}), note: json!({"token": token, "error": message}) }
}

fn blob(ctx: &Ctx, service: &str, token: &str) -> std::path::PathBuf {
    ctx.path("restpki").join(format!("{service}-{token}.pdf"))
}

pub async fn run(ctx: &Ctx, p: &Plugin, native: &str, params: &Value, body: &Value) -> Native {
    let svc = p.service.name.as_str();
    let token = params["token"].as_str().unwrap_or("").to_string();
    if native == "restpki.start" {
        return start(ctx, svc, body).await;
    }
    let Some(mut s) = store::kept(ctx, svc, SESSIONS, &token).await.ok().flatten() else {
        return fail("token_not_found", "TokenNotFound", format!("Token {token} is not a REST PKI signature here"), &token);
    };
    match native {
        "restpki.webpki_start" => {
            let Some(cert) = gi(body, &["certificate"]).and_then(Value::as_str).and_then(|c| B64.decode(c.trim()).ok()) else {
                return fail("validation_error", "ValidationError", "certificate (DER, base64) is required".into(), &token);
            };
            let digest = B64.decode(s["digest"].as_str().unwrap_or("")).unwrap_or_default();
            let attrs = match pades::signed_attrs(&digest, &cert) {
                Ok(a) => a,
                Err(e) => return fail("validation_error", "ValidationError", e, &token),
            };
            let extra: Vec<Vec<u8>> = gi(body, &["chain"]).and_then(Value::as_array).into_iter().flatten().filter_map(|c| c.as_str().and_then(|c| B64.decode(c.trim()).ok())).collect();
            let chain = chain(p, &cert, extra);
            let thumb = hex::encode(sha1::Sha1::digest(&cert));
            let cn = cert_model(&cert)["subjectName"]["commonName"].clone();
            s["status"] = json!("prepared");
            s["certificate"] = json!(B64.encode(&cert));
            s["chain"] = json!(chain.iter().map(|c| B64.encode(c)).collect::<Vec<_>>());
            s["signedAttrs"] = json!(B64.encode(&attrs));
            s["thumbprint"] = json!(thumb);
            s["signer"] = cn.clone();
            let _ = store::keep(ctx, svc, SESSIONS, &token, s).await;
            Native { answer: "to_sign", result: json!({"toSignData": B64.encode(&attrs), "digestAlgorithm": "SHA-256"}), note: json!({"token": token, "thumbprint": thumb, "signer": cn, "chain": chain.len()}) }
        }
        "restpki.webpki_signature" => {
            let (Some(attrs), Some(cert)) = (s["signedAttrs"].as_str().and_then(|a| B64.decode(a).ok()), s["certificate"].as_str().and_then(|c| B64.decode(c).ok())) else {
                return fail("validation_error", "ValidationError", "The browser has not started this signature (__webpki/{token}/start)".into(), &token);
            };
            let sig = gi(body, &["signature"]).and_then(Value::as_str).and_then(|x| B64.decode(x.trim()).ok()).unwrap_or_default();
            let thumb = s["thumbprint"].clone();
            if !pades::verify(&cert, &attrs, &sig) {
                return Native { answer: "invalid_signature", result: json!({"code": "InvalidSignature", "message": "The signature does not match the certificate", "detail": "RSA PKCS#1 v1.5 SHA-256 over the signed attributes"}), note: json!({"token": token, "thumbprint": thumb, "valid": false}) };
            }
            s["status"] = json!("signed");
            s["signature"] = json!(B64.encode(&sig));
            let _ = store::keep(ctx, svc, SESSIONS, &token, s).await;
            Native { answer: "signature_ok", result: json!({"ok": true}), note: json!({"token": token, "thumbprint": thumb, "valid": true}) }
        }
        _ => {
            if s["signature"].as_str().is_none() {
                return fail("signature_not_found", "SignatureNotFound", "The Web PKI never signed this token".into(), &token);
            }
            let d = |k: &str| B64.decode(s[k].as_str().unwrap_or("")).unwrap_or_default();
            let (cert, attrs, sig) = (d("certificate"), d("signedAttrs"), d("signature"));
            let chain: Vec<Vec<u8>> = s["chain"].as_array().into_iter().flatten().filter_map(|c| c.as_str().and_then(|c| B64.decode(c).ok())).collect();
            let contents = (s["contents"][0].as_u64().unwrap_or(0) as usize, s["contents"][1].as_u64().unwrap_or(0) as usize);
            let file = blob(ctx, svc, &token);
            let signed = std::fs::read(&file).map_err(|e| format!("prepared PDF: {e}")).and_then(|pdf| pades::cms(&attrs, &sig, &cert, &chain).and_then(|c| pades::embed(&pdf, contents, &c)));
            let signed = match signed {
                Ok(x) => x,
                Err(e) => return fail("validation_error", "ValidationError", e, &token),
            };
            let _ = std::fs::remove_file(file);
            s["status"] = json!("finished");
            s["signedBytes"] = json!(signed.len());
            let thumb = s["thumbprint"].clone();
            let _ = store::keep(ctx, svc, SESSIONS, &token, s).await;
            Native { answer: "signed", result: json!({"signatureFile": {"mimeType": "application/pdf", "content": B64.encode(&signed)}, "certificate": cert_model(&cert), "callbackArgument": null}),
                note: json!({"token": token, "thumbprint": thumb, "signedBytes": signed.len()}) }
        }
    }
}

async fn start(ctx: &Ctx, svc: &str, body: &Value) -> Native {
    let Some(pdf) = gi(body, &["pdfToSign", "content"]).and_then(Value::as_str).and_then(|c| B64.decode(c.trim()).ok()) else {
        return fail("validation_error", "ValidationError", "pdfToSign.content (base64) is required".into(), "");
    };
    let vr = gi(body, &["visualRepresentation"]).cloned().unwrap_or(Value::Null);
    let pos = gi(&vr, &["position"]).cloned().unwrap_or(Value::Null);
    let man = gi(&pos, &["manual"]).or(gi(&pos, &["manualPositioning"])).cloned().unwrap_or(Value::Null);
    let points = gi(&pos, &["measurementUnits"]).and_then(Value::as_str).map(|u| u.eq_ignore_ascii_case("PdfPoints")).unwrap_or(false);
    let k = if points { 1.0 } else { pades::CM_TO_PT };
    let png = gi(&vr, &["image", "resource", "content"]).and_then(Value::as_str).and_then(|c| B64.decode(c.trim()).ok());
    let page = num(&pos, &["pageNumber"]) as i64;
    let cm = json!({"left": num(&man, &["left"]), "bottom": num(&man, &["bottom"]), "width": num(&man, &["width"]), "height": num(&man, &["height"])});
    let v = pades::Visual { page: if page == 0 { 1 } else { page }, left: num(&man, &["left"]) * k, bottom: num(&man, &["bottom"]) * k, width: num(&man, &["width"]) * k, height: num(&man, &["height"]) * k, png: png.clone() };
    let token = util::uuid().replace('-', "");
    let prep = match pades::prepare(&pdf, &v, &format!("Signature-{}", &token[..8]), util::now_ms() / 1000) {
        Ok(p) => p,
        Err(e) => return fail("validation_error", "ValidationError", e, ""),
    };
    let file = blob(ctx, svc, &token);
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    if let Err(e) = std::fs::write(&file, &prep.pdf) {
        return fail("validation_error", "ValidationError", format!("Stuntman could not keep the PDF: {e}"), "");
    }
    let note = json!({"token": token, "page": prep.page, "position": cm, "rect": prep.rect.iter().map(|x| (x * 100.0).round() / 100.0).collect::<Vec<_>>(), "image": png.is_some(), "pdfBytes": pdf.len(),
        "policy": gi(body, &["signaturePolicyId"]), "securityContext": gi(body, &["securityContextId"])});
    let mut s = note.clone();
    s["at"] = json!(util::now_ms());
    s["status"] = json!("started");
    s["contents"] = json!([prep.contents.0, prep.contents.1]);
    s["digest"] = json!(B64.encode(&prep.digest));
    let _ = store::keep(ctx, svc, SESSIONS, &token, s).await;
    Native { answer: "started", result: json!({"token": token, "certificate": null, "toSignData": null, "toSignHash": null, "digestAlgorithmOid": null}), note }
}

/// The signer's issuers: the plugin's `issuers` table (PEM or base64 DER), the Stuntman Test CA and
/// what the browser sent, walked from the leaf up.
fn chain(p: &Plugin, leaf: &[u8], mut pool: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    for i in p.data.get("issuers").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        if i.contains("BEGIN CERTIFICATE") {
            pool.extend(crate::ca::der_of(i).into_iter().map(|d| d.to_vec()));
        } else if let Ok(d) = B64.decode(i.chars().filter(|c| !c.is_whitespace()).collect::<String>()) {
            pool.push(d);
        }
    }
    pool.extend(crate::ca::der_of(&crate::ca::get().pem).into_iter().map(|d| d.to_vec()));
    let mut out: Vec<Vec<u8>> = vec![];
    let mut cur = leaf.to_vec();
    while out.len() < 5 {
        let Ok(ci) = pades::cert_info(&cur) else { break };
        if ci.issuer == ci.subject {
            break;
        }
        let Some(next) = pool.iter().find(|c| pades::cert_info(c).map(|x| x.subject == ci.issuer).unwrap_or(false) && !out.contains(c)).cloned() else { break };
        out.push(next.clone());
        cur = next;
    }
    out
}

fn iso(t: x509_parser::time::ASN1Time) -> String {
    let d = t.to_datetime();
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00", d.year(), d.month() as u8, d.day(), d.hour(), d.minute(), d.second())
}

fn name(n: &X509Name) -> Value {
    let first = |it: &mut dyn Iterator<Item = &x509_parser::x509::AttributeTypeAndValue>| it.next().and_then(|a| a.as_str().ok()).map(String::from);
    let mut all: Map<String, Value> = Map::new();
    for a in n.iter_attributes() {
        if let Ok(s) = a.as_str() {
            all.entry(a.attr_type().to_id_string()).or_insert(json!([])).as_array_mut().unwrap().push(json!(s));
        }
    }
    json!({"commonName": first(&mut n.iter_common_name()), "country": first(&mut n.iter_country()).unwrap_or_default(), "organization": first(&mut n.iter_organization()).unwrap_or_default(), "allValues": all})
}

/// REST PKI's CertificateModel (the .NET PKCertificate needs allValues and pkiBrazil).
pub fn cert_model(der: &[u8]) -> Value {
    let Ok((_, c)) = X509Certificate::from_der(der) else { return Value::Null };
    let cn = c.subject().iter_common_name().next().and_then(|a| a.as_str().ok()).unwrap_or("").to_string();
    let (who, cpf) = match cn.rsplit_once(':') { Some((a, b)) => (a.to_string(), Some(b.to_string())), None => (cn.clone(), None) };
    json!({"subjectName": name(c.subject()), "issuerName": name(c.issuer()), "emailAddress": null, "serialNumber": c.tbs_certificate.serial.to_string(),
        "validityStart": iso(c.validity().not_before), "validityEnd": iso(c.validity().not_after),
        "pkiBrazil": {"cpf": cpf, "responsavel": who, "certificateType": "A1"}, "pkiItaly": {},
        "thumbprint": hex::encode(sha1::Sha1::digest(der)), "binaryThumbprintSHA256": B64.encode(<sha2::Sha256 as sha2::Digest>::digest(der))})
}

// ---------------------------------------------------------------- tools

fn service(a: &Args) -> String {
    a.str("service").unwrap_or("restpki".into())
}

async fn sessions(ctx: &Ctx, svc: &str) -> Result<Vec<Value>, ApiError> {
    let st = store::state(ctx, svc).await?;
    let mut v: Vec<Value> = st[SESSIONS].as_object().map(|o| o.values().cloned().collect()).unwrap_or_default();
    for s in v.iter_mut() {
        if let Some(o) = s.as_object_mut() {
            for k in ["digest", "contents", "signedAttrs", "signature", "certificate", "chain"] {
                o.remove(k);
            }
        }
    }
    v.sort_by_key(|s| -s["at"].as_i64().unwrap_or(0));
    Ok(v)
}

pub fn tools() -> Vec<Tool> {
    vec![
        Tool::new("list_restpki_sessions", "REST PKI signatures, newest first: token, status (started | prepared | signed | finished), page, position (cm), stamp image, signer and thumbprint.", |ctx, a| async move {
            Ok(json!({"sessions": sessions(&ctx, &service(&a)).await?}))
        })
        .param("service", "the plugin, default restpki")
        .route("GET", "/apis/{service}/sessions"),
        Tool::new("get_restpki_session", "One REST PKI signature by token, with the certificate and its chain (base64 DER) once the browser started it.", |ctx, a| async move {
            let (svc, t) = (service(&a), a.need("token")?);
            let mut s = store::kept(&ctx, &svc, SESSIONS, &t).await?.ok_or_else(|| ApiError::not_found(format!("No REST PKI signature {t}")))?;
            if let Some(c) = s["certificate"].as_str().and_then(|c| B64.decode(c).ok()) {
                s["certificateModel"] = cert_model(&c);
            }
            Ok(s)
        })
        .req("token", "the token").param("service", "the plugin, default restpki")
        .route("GET", "/apis/{service}/sessions/{token}"),
        Tool::new("fail_restpki", "The next call of a step fails: step=start (the app's POST PadesSignatures) | webpki_start | webpki_signature | finish (SignedBytes); status=422 (ValidationError, default) or 500; count=N calls (default 1). status=default stops.", |ctx, a| async move {
            let svc = service(&a);
            let p = crate::plugin::get(&ctx, &svc)?;
            let step = a.need("step")?;
            let native = format!("restpki.{step}");
            let route = p.routes.iter().find(|r| r.native == native).map(|r| { let (m, pat) = r.method_path(); format!("{m} {pat}") })
                .ok_or_else(|| ApiError::bad(format!("step: start | webpki_start | webpki_signature | finish (no route of {svc} runs {native})")))?;
            let answer = match a.str("status").unwrap_or("422".into()).as_str() { "422" => "validation_error", "500" => "500", "default" => "default", s => return Err(ApiError::bad(format!("status {s}: 422 | 500 | default"))) };
            let by = a.str("_by").unwrap_or("you".into());
            store::force(&ctx, &svc, answer, &route, Some(a.u64("count").unwrap_or(1) as i64), None, &by).await?;
            Ok(json!({"service": svc, "route": route, "forced": store::forced(&ctx, &svc).await?}))
        })
        .req("step", "start | webpki_start | webpki_signature | finish").param("status", "422 (default) | 500 | default").param("count", "next N calls (default 1)").param("service", "the plugin, default restpki")
        .route("POST", "/apis/{service}/fail"),
        Tool::new("reset_restpki", "Forgets every REST PKI signature of the service and stops a forced failure (calls stay in the history).", |ctx, a| async move {
            let svc = service(&a);
            let n = sessions(&ctx, &svc).await?.len();
            store::wipe_service(&ctx, &svc).await?;
            for e in std::fs::read_dir(ctx.path("restpki")).into_iter().flatten().flatten() {
                if e.file_name().to_string_lossy().starts_with(&format!("{svc}-")) {
                    let _ = std::fs::remove_file(e.path());
                }
            }
            ctx.live.send("apis.force", &svc, "updated");
            Ok(json!({"service": svc, "forgotten": n}))
        })
        .param("service", "the plugin, default restpki")
        .route("POST", "/apis/{service}/reset"),
    ]
}
