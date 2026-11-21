//! Checks a delivery the way a careful customer would: signature, timestamp age, replay.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::config::Inbox;

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// valid | invalid | old_secret | missing | unchecked
    pub result: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp_age_ms: Option<i64>,
    pub header: String,
    /// A weakness of the scheme the app uses, even when the signature is valid.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub warning: String,
}

fn mac(secret: &str, msg: &[u8]) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

fn find<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a (String, String)> {
    headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name))
}

pub fn header_like<'a>(headers: &'a [(String, String)], part: &str) -> Option<&'a (String, String)> {
    headers.iter().find(|(k, _)| k.to_lowercase().contains(part))
}

fn matches(scheme: &str, secret: &str, value: &str, body: &[u8], t: Option<&str>) -> bool {
    if secret.is_empty() {
        return false;
    }
    match scheme {
        "t-v1-hmac-sha256" => {
            let Some(t) = t else { return false };
            let mut msg = format!("{t}.").into_bytes();
            msg.extend_from_slice(body);
            let want = hex::encode(mac(secret, &msg));
            value.split(',').filter_map(|p| p.trim().strip_prefix("v1=")).any(|v| v.eq_ignore_ascii_case(&want))
        }
        "hmac-sha256-hex" => value.trim().trim_start_matches("sha256=").eq_ignore_ascii_case(&hex::encode(mac(secret, body))),
        "hmac-sha256-base64" => value.trim() == B64.encode(mac(secret, body)),
        _ => false,
    }
}

pub fn check(i: &Inbox, headers: &[(String, String)], body: &[u8], now_ms: i64, data_dir: &std::path::Path) -> Check {
    let sig = &i.signature;
    let secret = i.secret_in(data_dir);
    let previous = crate::config::resolve(&sig.previous_secret, data_dir);
    if sig.scheme == "none" || secret.is_empty() {
        let why = if sig.secret.starts_with("secret://") { format!("{} is not in secrets.yaml yet", sig.secret) } else { "no secret set for this inbox".into() };
        return Check { result: "unchecked".into(), detail: why, ..Default::default() };
    }
    let h = if sig.header.is_empty() { header_like(headers, "signature") } else { find(headers, &sig.header) };
    let Some((hname, value)) = h else {
        return Check { result: "missing".into(), detail: "no signature header".into(), ..Default::default() };
    };
    let t = if sig.scheme == "t-v1-hmac-sha256" {
        value.split(',').find_map(|p| p.trim().strip_prefix("t=")).map(String::from)
    } else {
        header_like(headers, "timestamp").map(|(_, v)| v.trim().to_string())
    };
    let age = t.as_deref().and_then(|t| t.parse::<i64>().ok()).map(|t| now_ms - if t > 10_000_000_000 { t } else { t * 1000 });
    let what = match sig.scheme.as_str() {
        "t-v1-hmac-sha256" => "HMAC-SHA256 over “t.body”",
        "hmac-sha256-base64" => "HMAC-SHA256 of the body, base64",
        _ => "HMAC-SHA256 of the body, hex",
    };
    // A timestamp header next to a body-only signature can be changed freely: the age check it allows
    // does not stop a replay. Say so, as a careful customer would.
    let unsigned_t = sig.scheme != "t-v1-hmac-sha256" && t.is_some();
    let (result, detail) = if matches(&sig.scheme, &secret, value, body, t.as_deref()) {
        let base = match sig.secret.strip_prefix("secret://") { Some(n) => format!("valid · {what} with Secrets › {n}"), None => format!("valid · {what}") };
        ("valid", base)
    } else if matches(&sig.scheme, &previous, value, body, t.as_deref()) {
        ("old_secret", "signed with the old secret".to_string())
    } else {
        ("invalid", format!("does not match · {what}"))
    };
    // A right signature on a timestamp older than the tolerance: what Stripe's and Svix's libraries refuse.
    let tol = i.tolerance_ms() as i64;
    let (result, detail) = match age {
        Some(a) if result == "valid" && a > tol => ("stale", format!("valid signature, but the timestamp is {} s old (tolerance {})", a / 1000, sig.tolerance)),
        _ => (result, detail),
    };
    let warning = if unsigned_t { "the timestamp is not signed: a replay with a new timestamp still passes".to_string() } else { String::new() };
    Check { result: result.into(), detail, timestamp_age_ms: age, header: hname.clone(), warning }
}

#[cfg(test)]
mod t {
    use super::*;

    fn inbox() -> Inbox {
        let mut i = Inbox::new("globex");
        i.signature.secret = "whsec_new".into();
        i.signature.previous_secret = "whsec_old".into();
        i
    }

    fn sign(secret: &str, t: &str, body: &[u8]) -> String {
        let mut msg = format!("{t}.").into_bytes();
        msg.extend_from_slice(body);
        format!("t={t},v1={}", hex::encode(mac(secret, &msg)))
    }

    #[test]
    fn secret_reference_resolves_from_the_data_folder() {
        let dir = std::env::temp_dir().join(format!("stuntman-sec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("secrets.yaml"), "initech-webhook-secret: whsec_new\n").unwrap();
        let mut i = inbox();
        i.signature.secret = "secret://initech-webhook-secret".into();
        let body = b"{}";
        let c = check(&i, &[("X-Sig-Signature".into(), sign("whsec_new", "1794540729", body))], body, 1_794_540_730_000, &dir);
        assert_eq!(c.result, "valid");
        assert!(c.detail.contains("Secrets › initech-webhook-secret"));
    }

    #[test]
    fn valid_old_and_bad() {
        let body = br#"{"type":"order.completed"}"#;
        let now = 1_794_540_730_000;
        let h = |v: String| vec![("X-Acme-Signature".to_string(), v)];
        let c = check(&inbox(), &h(sign("whsec_new", "1794540729", body)), body, now, std::path::Path::new("/nonexistent"));
        assert_eq!((c.result.as_str(), c.timestamp_age_ms), ("valid", Some(1000)));
        assert_eq!(check(&inbox(), &h(sign("whsec_old", "1794540729", body)), body, now, std::path::Path::new("/nonexistent")).result, "old_secret");
        assert_eq!(check(&inbox(), &h(sign("x", "1794540729", body)), body, now, std::path::Path::new("/nonexistent")).result, "invalid");
        assert_eq!(check(&inbox(), &[], body, now, std::path::Path::new("/nonexistent")).result, "missing");
        let mut g = inbox();
        g.signature.scheme = "hmac-sha256-hex".into();
        let v = format!("sha256={}", hex::encode(mac("whsec_new", body)));
        assert_eq!(check(&g, &[("X-Hub-Signature-256".into(), v.clone())], body, now, std::path::Path::new("/nonexistent")).result, "valid");
        // A timestamp header beside a body-only signature is flagged: it is not covered.
        let c = check(&g, &[("X-Acme-Signature".into(), v), ("X-Acme-Timestamp".into(), "1794540729".into())], body, now, std::path::Path::new("/nonexistent"));
        assert_eq!((c.result.as_str(), c.timestamp_age_ms), ("valid", Some(1000)));
        assert!(c.warning.contains("timestamp is not signed"));
    }
}
