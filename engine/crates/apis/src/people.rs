//! Synthetic people of a plugin (data.people) with their cloud certificate: an RSA key kept in
//! data/keys/people and a certificate the Test CA issues (valid, or expired on purpose). Plugins read
//! them through cert_pem(person) and sign_all(person, hashes, encoding): a RAW PKCS#1 v1.5 signature
//! over the hash the app sends, which the app can verify against the certificate.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rsa::{pkcs8::{DecodePrivateKey, EncodePrivateKey, LineEnding}, Pkcs1v15Sign, RsaPrivateKey};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::{ca, expr};

fn key_of(cpf: &str) -> Result<RsaPrivateKey, String> {
    let dir = ca::keys_dir().join("people");
    let path = dir.join(format!("{cpf}.key.pem"));
    if let Some(k) = std::fs::read_to_string(&path).ok().and_then(|t| RsaPrivateKey::from_pkcs8_pem(&t).ok()) {
        return Ok(k);
    }
    let k = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).map_err(|e| e.to_string())?;
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&path, k.to_pkcs8_pem(LineEnding::LF).map_err(|e| e.to_string())?.as_bytes());
    Ok(k)
}

fn pem_block(der: &[u8]) -> String {
    let b = B64.encode(der);
    let lines: Vec<&str> = b.as_bytes().chunks(64).map(|c| std::str::from_utf8(c).unwrap_or("")).collect();
    format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", lines.join("\n"))
}

fn cpf(person: &Value) -> Result<String, String> {
    let c: String = expr::text(&person["cpf"]).chars().filter(|c| c.is_ascii_digit()).collect();
    if c.is_empty() { Err("the person has no cpf".into()) } else { Ok(c) }
}

/// Leaf + Test CA, PEM. certificate: valid (default) | expired | none.
pub fn cert_pem(person: &Value) -> Result<String, String> {
    let c = cpf(person)?;
    let state = person["certificate"].as_str().unwrap_or("valid");
    if state == "none" {
        return Err(format!("{} has no certificate", expr::text(&person["name"])));
    }
    // One issued with issue_person_certificate wins over the plugin's valid/expired.
    for path in [window_file(&c), ca::keys_dir().join("people").join(format!("{c}.{state}.pem"))] {
        if let Ok(t) = std::fs::read_to_string(&path) {
            return Ok(t);
        }
    }
    let path = ca::keys_dir().join("people").join(format!("{c}.{state}.pem"));
    let k = key_of(&c)?;
    let der = k.to_pkcs8_der().map_err(|e| e.to_string())?;
    let cn = format!("{}:{c}", expr::text(&person["name"]).to_uppercase());
    let (from, days) = if state == "expired" { (-400, 365) } else { (-30, 730) };
    let leaf = ca::get().issue(&cn, &[], der.as_bytes(), from, days, false)?;
    let pem = format!("{}{}", pem_block(&leaf), ca::get().pem.trim_end().to_string() + "\n");
    let _ = std::fs::write(&path, &pem);
    Ok(pem)
}

fn window_file(cpf: &str) -> std::path::PathBuf {
    ca::keys_dir().join("people").join(format!("{cpf}.window.pem"))
}

/// (Re)issues the CPF's certificate with this validity window (Unix seconds), signed by the Test
/// CA with the person's own key, so their signatures still verify. Every plugin's people answer it
/// (BirdID, VIDaaS) until it is cleared. The keys never leave data/keys.
pub fn issue_window(cpf_raw: &str, name: &str, not_before: i64, not_after: i64) -> Result<Value, String> {
    let c = cpf(&json!({"cpf": cpf_raw}))?;
    if not_after <= not_before {
        return Err("not_after must come after not_before".into());
    }
    let der = key_of(&c)?.to_pkcs8_der().map_err(|e| e.to_string())?;
    let cn = format!("{}:{c}", name.to_uppercase());
    let leaf = ca::get().issue_between(&cn, &[], der.as_bytes(), not_before, not_after, false)?;
    let pem = format!("{}{}", pem_block(&leaf), ca::get().pem.trim_end().to_string() + "\n");
    std::fs::write(window_file(&c), &pem).map_err(|e| e.to_string())?;
    let now = stuntman_core::util::now_ms() / 1000;
    let state = if now > not_after { "expired" } else if now < not_before { "not_yet_valid" } else { "valid" };
    Ok(json!({"cpf": c, "subject": cn, "state": state, "certificate": pem}))
}

/// Back to the plugin's certificate (valid or expired as its people table says).
pub fn clear_window(cpf_raw: &str) -> Result<bool, String> {
    let c = cpf(&json!({"cpf": cpf_raw}))?;
    Ok(std::fs::remove_file(window_file(&c)).is_ok())
}

/// RAW PKCS#1 v1.5 (SHA-256 DigestInfo) over one hash, given as hex or base64; base64 out.
pub fn sign_raw(person: &Value, hash: &str, enc: &str) -> Result<String, String> {
    cert_pem(person)?;
    let digest = if enc == "hex" { hex::decode(hash.trim()).map_err(|_| "the hash is not hex".to_string())? } else { B64.decode(hash.trim()).map_err(|_| "the hash is not base64".to_string())? };
    if digest.len() != 32 {
        return Err(format!("a SHA-256 hash has 32 bytes, this one {}", digest.len()));
    }
    let k = key_of(&cpf(person)?)?;
    k.sign(Pkcs1v15Sign::new::<Sha256>(), &digest).map(|s| B64.encode(s)).map_err(|e| e.to_string())
}

/// [{id, raw_signature}] for every {id, hash} the app sent.
pub fn sign_all(person: &Value, hashes: &Value, enc: &str) -> Result<Value, String> {
    let list = hashes.as_array().ok_or("hashes must be a list")?;
    list.iter().map(|h| Ok(json!({"id": h["id"], "raw_signature": sign_raw(person, &expr::text(&h["hash"]), enc)?}))).collect::<Result<Vec<_>, String>>().map(Value::Array)
}

#[cfg(test)]
mod t {
    use super::*;
    use rsa::{pkcs8::DecodePublicKey, RsaPublicKey};

    #[test]
    fn signs_with_the_certificate_key() {
        let dir = std::env::temp_dir().join(format!("stuntman-people-{}", std::process::id()));
        ca::set_dir(&dir);
        let p = json!({"name": "Emma Carter", "cpf": "123.456.789-09"});
        let pem = cert_pem(&p).unwrap();
        assert_eq!(pem.matches("BEGIN CERTIFICATE").count(), 2);
        let digest = <sha2::Sha256 as sha2::Digest>::digest(b"doc");
        let s = B64.decode(sign_raw(&p, &hex::encode(digest), "hex").unwrap()).unwrap();
        let pubk = RsaPublicKey::from(&key_of("12345678909").unwrap());
        assert!(pubk.verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &s).is_ok());
        assert!(sign_raw(&json!({"name": "N", "cpf": "1", "certificate": "none"}), &hex::encode(digest), "hex").is_err());
        let _ = RsaPublicKey::from_public_key_pem("");
    }
}
