//! A14: a provider as a SAML 2.0 identity provider. Metadata with its signing certificate, SSO by
//! HTTP-Redirect (deflated) and HTTP-POST, the same sign-in page as OIDC, and a SAMLResponse posted
//! to the app's ACS with the assertion signed (RSA-SHA256, exclusive c14n). The app (service
//! provider) is an app registration whose client_id is its entity ID and whose redirect URIs are its
//! ACS URLs.

use std::{collections::HashMap, io::Read};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    Form,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Ctx};

use crate::{
    config::{App, Provider, User},
    flow, page, store, token,
};

pub fn entity_id(ctx: &Ctx, p: &Provider) -> String {
    format!("{}/saml", token::base(ctx, p))
}

fn iso(ms: i64) -> String {
    stuntman_apis::expr::iso(ms, None).split('.').next().unwrap_or("").trim_end_matches('Z').to_string() + "Z"
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The signing key and its certificate (kept, so metadata and responses show the same one).
pub async fn signer(ctx: &Ctx, p: &mut Provider) -> Result<(String, Vec<u8>), ApiError> {
    token::ensure_keys(ctx, p).await?;
    let kid = p.keys.current.clone();
    let pem = store::key(ctx, &p.name, &kid).await?.ok_or_else(|| ApiError::internal("no signing key"))?;
    if let Some((v, ..)) = store::get(ctx, "saml_cert", &format!("{}:{kid}", p.name)).await? {
        if let Some(der) = v["der"].as_str().and_then(|d| B64.decode(d).ok()) {
            return Ok((pem, der));
        }
    }
    let pkcs8 = stuntman_keyvault::crypto::pkcs8_der(&pem).map_err(ApiError::internal)?;
    let der = stuntman_apis::ca::get().issue(&format!("{} SAML signing", p.name), &[], &pkcs8, -1, 3650, false).map_err(ApiError::internal)?;
    store::put(ctx, "saml_cert", &format!("{}:{kid}", p.name), &p.name, &json!({"der": B64.encode(&der)})).await?;
    Ok((pem, der))
}

pub async fn metadata(State(ctx): State<Ctx>, Path(tenant): Path<String>) -> Response {
    let Some(mut p) = crate::config::by_tenant(&ctx, &tenant).filter(|p| p.saml.is_some()) else {
        return (StatusCode::NOT_FOUND, "Stuntman: no identity provider with SAML for this tenant (Identity › Protocols)").into_response();
    };
    let Ok((_, der)) = signer(&ctx, &mut p).await else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
    let sso = format!("{}/saml/sso", token::base(&ctx, &p));
    let xml = format!(r#"<?xml version="1.0" encoding="UTF-8"?><md:EntityDescriptor xmlns:md="urn:oasis:names:tc:SAML:2.0:metadata" entityID="{}"><md:IDPSSODescriptor WantAuthnRequestsSigned="false" protocolSupportEnumeration="urn:oasis:names:tc:SAML:2.0:protocol"><md:KeyDescriptor use="signing"><ds:KeyInfo xmlns:ds="http://www.w3.org/2000/09/xmldsig#"><ds:X509Data><ds:X509Certificate>{}</ds:X509Certificate></ds:X509Data></ds:KeyInfo></md:KeyDescriptor><md:NameIDFormat>urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress</md:NameIDFormat><md:NameIDFormat>urn:oasis:names:tc:SAML:2.0:nameid-format:persistent</md:NameIDFormat><md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-Redirect" Location="{sso}"/><md:SingleSignOnService Binding="urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST" Location="{sso}"/></md:IDPSSODescriptor></md:EntityDescriptor>"#,
        esc(&entity_id(&ctx, &p)), B64.encode(&der));
    ([("content-type", "application/samlmetadata+xml")], xml).into_response()
}

/// The AuthnRequest: (ID, ACS URL, the SP's entity ID).
pub fn parse_request(raw_b64: &str, deflated: bool) -> Result<(String, String, String), String> {
    let bytes = B64.decode(raw_b64.trim().replace(' ', "+")).map_err(|_| "SAMLRequest is not base64")?;
    let xml = if deflated {
        let mut s = String::new();
        flate2::read::DeflateDecoder::new(&bytes[..]).read_to_string(&mut s).map_err(|e| format!("SAMLRequest is not deflated: {e}"))?;
        s
    } else {
        String::from_utf8(bytes).map_err(|_| "SAMLRequest is not UTF-8")?
    };
    let doc = roxmltree::Document::parse(&xml).map_err(|e| format!("SAMLRequest is not XML: {e}"))?;
    let r = doc.root_element();
    if r.tag_name().name() != "AuthnRequest" {
        return Err(format!("expected an AuthnRequest, got {}", r.tag_name().name()));
    }
    let issuer = r.children().find(|c| c.tag_name().name() == "Issuer").and_then(|c| c.text()).unwrap_or("").trim().to_string();
    Ok((r.attribute("ID").unwrap_or("").to_string(), r.attribute("AssertionConsumerServiceURL").unwrap_or("").to_string(), issuer))
}

async fn sso(ctx: Ctx, tenant: String, q: HashMap<String, String>, deflated: bool) -> Response {
    let Some(mut p) = crate::config::by_tenant(&ctx, &tenant).filter(|p| p.saml.is_some()) else {
        return (StatusCode::NOT_FOUND, Html(page::error(None, "SAML", "No identity provider with SAML for this tenant (Identity › Protocols).", None))).into_response();
    };
    let _ = token::ensure_keys(&ctx, &mut p).await;
    let Some(req) = q.get("SAMLRequest") else { return (StatusCode::BAD_REQUEST, Html(page::error(Some(&p), "SAML", "SAMLRequest is missing.", None))).into_response() };
    let (id, acs, sp) = match parse_request(req, deflated) {
        Ok(x) => x,
        Err(e) => return (StatusCode::BAD_REQUEST, Html(page::error(Some(&p), "SAML", &e, None))).into_response(),
    };
    let qv = json!({"client_id": sp, "redirect_uri": acs, "scope": "", "state": q.get("RelayState").cloned().unwrap_or_default(), "nonce": "", "response_type": "saml",
        "response_mode": "form_post", "saml_request_id": id, "prompt": "", "login_hint": ""});
    match flow::start(&ctx, &p, qv).await {
        Ok(pid) => {
            let d = store::get(&ctx, "pending", &pid).await.ok().flatten().map(|x| x.0).unwrap_or_default();
            Html(page::pick(&p, &pid, &d, &ctx.env, "../oauth2/v2.0/authorize")).into_response()
        }
        Err(flow::Next::Error { code, message, .. }) => (StatusCode::BAD_REQUEST, Html(page::error(Some(&p), &code, &message, None))).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

pub async fn sso_get(State(ctx): State<Ctx>, Path(tenant): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    sso(ctx, tenant, q, true).await
}

pub async fn sso_post(State(ctx): State<Ctx>, Path(tenant): Path<String>, Form(f): Form<HashMap<String, String>>) -> Response {
    sso(ctx, tenant, f, false).await
}

/// The SAMLResponse for a finished sign-in (or a refusal: status Responder / RequestDenied).
pub async fn response(ctx: &Ctx, p: &mut Provider, d: &Value, user: Option<&User>, app: &App, twist: &str) -> Result<String, ApiError> {
    let now = util::now_ms();
    let issuer = entity_id(ctx, p);
    let acs = d["redirect_uri"].as_str().unwrap_or("");
    let in_resp = d["saml_request_id"].as_str().unwrap_or("");
    let rid = format!("_{}", util::uuid().replace('-', ""));
    let aid = format!("_{}", util::uuid().replace('-', ""));
    let in_resp_attr = if in_resp.is_empty() { String::new() } else { format!(" InResponseTo=\"{}\"", esc(in_resp)) };
    let Some(u) = user else {
        let xml = format!(r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="{rid}" Version="2.0" IssueInstant="{}" Destination="{}"{in_resp_attr}><saml:Issuer>{}</saml:Issuer><samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Responder"><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:RequestDenied"/></samlp:StatusCode><samlp:StatusMessage>The person cancelled the sign-in</samlp:StatusMessage></samlp:Status></samlp:Response>"#,
            iso(now), esc(acs), esc(&issuer));
        return Ok(B64.encode(xml));
    };
    let cfg = p.saml.clone().unwrap_or_default();
    let (nid_fmt, nid) = match cfg.name_id.as_str() {
        "oid" => ("urn:oasis:names:tc:SAML:2.0:nameid-format:persistent", u.oid.clone()),
        "upn" => ("urn:oasis:names:tc:SAML:1.1:nameid-format:unspecified", u.email.clone()),
        _ => ("urn:oasis:names:tc:SAML:1.1:nameid-format:emailAddress", u.email.clone()),
    };
    let expired = twist == "expired";
    let (nb, na) = if expired { (now - 7_200_000, now - 3_600_000) } else { (now - 60_000, now + 3_600_000) };
    let audience = if twist == "wrong_audience" { "urn:someone-else".to_string() } else { app.client_id.clone() };
    let attr = |name: &str, vals: &[String]| format!("<saml:Attribute Name=\"{}\" NameFormat=\"urn:oasis:names:tc:SAML:2.0:attrname-format:uri\">{}</saml:Attribute>", name, vals.iter().map(|v| format!("<saml:AttributeValue>{}</saml:AttributeValue>", esc(v))).collect::<String>());
    let (given, sn) = u.name.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((u.name.clone(), String::new()));
    let mut attrs = vec![
        attr("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/emailaddress", std::slice::from_ref(&u.email)),
        attr("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/name", std::slice::from_ref(&u.email)),
        attr("http://schemas.microsoft.com/identity/claims/displayname", std::slice::from_ref(&u.name)),
        attr("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/givenname", &[given]),
        attr("http://schemas.xmlsoap.org/ws/2005/05/identity/claims/surname", &[sn]),
        attr("http://schemas.microsoft.com/identity/claims/objectidentifier", std::slice::from_ref(&u.oid)),
        attr("http://schemas.microsoft.com/identity/claims/tenantid", std::slice::from_ref(&p.tenant_id)),
    ];
    if !u.groups.is_empty() {
        attrs.push(attr("http://schemas.microsoft.com/ws/2008/06/identity/claims/groups", &u.groups));
    }
    if !u.roles.is_empty() {
        attrs.push(attr("http://schemas.microsoft.com/ws/2008/06/identity/claims/role", &u.roles));
    }
    let assertion = format!(r#"<saml:Assertion ID="{aid}" Version="2.0" IssueInstant="{now_s}"><saml:Issuer>{iss}</saml:Issuer><saml:Subject><saml:NameID Format="{nid_fmt}">{nid}</saml:NameID><saml:SubjectConfirmation Method="urn:oasis:names:tc:SAML:2.0:cm:bearer"><saml:SubjectConfirmationData{in_resp_attr} NotOnOrAfter="{na_s}" Recipient="{acs}"/></saml:SubjectConfirmation></saml:Subject><saml:Conditions NotBefore="{nb_s}" NotOnOrAfter="{na_s}"><saml:AudienceRestriction><saml:Audience>{aud}</saml:Audience></saml:AudienceRestriction></saml:Conditions><saml:AuthnStatement AuthnInstant="{now_s}" SessionIndex="{aid}"><saml:AuthnContext><saml:AuthnContextClassRef>urn:oasis:names:tc:SAML:2.0:ac:classes:PasswordProtectedTransport</saml:AuthnContextClassRef></saml:AuthnContext></saml:AuthnStatement><saml:AttributeStatement>{attrs}</saml:AttributeStatement></saml:Assertion>"#,
        now_s = iso(now), iss = esc(&issuer), nid = esc(&nid), na_s = iso(na), nb_s = iso(nb), acs = esc(acs), aud = esc(&audience), attrs = attrs.concat());
    let mut xml = format!(r#"<samlp:Response xmlns:samlp="urn:oasis:names:tc:SAML:2.0:protocol" xmlns:saml="urn:oasis:names:tc:SAML:2.0:assertion" ID="{rid}" Version="2.0" IssueInstant="{}" Destination="{}"{in_resp_attr}><saml:Issuer>{}</saml:Issuer><samlp:Status><samlp:StatusCode Value="urn:oasis:names:tc:SAML:2.0:status:Success"/></samlp:Status>{assertion}</samlp:Response>"#,
        iso(now), esc(acs), esc(&issuer));
    let (mut pem, der) = signer(ctx, p).await?;
    if twist == "bad_signature" {
        pem = stuntman_keyvault::crypto::generate(2048).map_err(ApiError::internal)?;
    }
    if cfg.sign_assertion {
        xml = stuntman_apis::xmldsig::sign(&xml, &aid, &pem, &der, false, true, stuntman_apis::xmldsig::Place::InsideAfter("Issuer"), "ds").map_err(ApiError::internal)?;
    }
    if cfg.sign_response {
        xml = stuntman_apis::xmldsig::sign(&xml, &rid, &pem, &der, false, true, stuntman_apis::xmldsig::Place::InsideAfter("Issuer"), "ds").map_err(ApiError::internal)?;
    }
    Ok(B64.encode(xml))
}
