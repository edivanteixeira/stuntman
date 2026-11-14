//! Calls Stuntman makes back to the app (provider event webhooks), signed like the provider does,
//! with the app's reply recorded so the screen can show "your app replied 200 in 91 ms".

use std::path::Path;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use p256::{
    ecdsa::{signature::Signer as _, DerSignature, SigningKey},
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding},
};
use serde::Serialize;

/// One ECDSA P-256 key per data folder, kept on disk so the app's configured public key survives
/// restarts. SendGrid's Event Webhook uses exactly this scheme.
pub struct Signer {
    key: SigningKey,
}

impl Signer {
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        if let Ok(pem) = std::fs::read_to_string(path) {
            if let Ok(key) = SigningKey::from_pkcs8_pem(&pem) {
                return Ok(Self { key });
            }
        }
        let key = SigningKey::random(&mut rand::thread_rng());
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(path, key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes())?;
        Ok(Self { key })
    }

    /// Base64 DER (SubjectPublicKeyInfo), the format SendGrid shows in its settings.
    pub fn public_key_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_public_key_der().unwrap().as_bytes())
    }

    /// Base64 DER signature of `timestamp + payload`.
    pub fn sign(&self, timestamp: &str, payload: &[u8]) -> String {
        let mut m = timestamp.as_bytes().to_vec();
        m.extend_from_slice(payload);
        let sig: DerSignature = self.key.sign(&m);
        B64.encode(sig.as_bytes())
    }
}

#[derive(Clone, Debug, Serialize, Default)]
pub struct Reply {
    pub status: Option<u16>,
    pub ms: u64,
    /// First 2 KB of the answer.
    pub body: String,
    /// Set when there was no HTTP answer (timeout, refused, bad URL).
    pub error: Option<String>,
}

pub async fn post(http: &reqwest::Client, url: &str, body: Vec<u8>, headers: &[(String, String)]) -> Reply {
    let t = std::time::Instant::now();
    let mut req = http.post(url).header("content-type", "application/json").timeout(std::time::Duration::from_secs(10)).body(body);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    match req.send().await {
        Ok(r) => {
            let status = r.status().as_u16();
            let mut body = r.text().await.unwrap_or_default();
            body.truncate(2048);
            Reply { status: Some(status), ms: t.elapsed().as_millis() as u64, body, error: None }
        }
        Err(e) => Reply {
            status: None,
            ms: t.elapsed().as_millis() as u64,
            body: String::new(),
            error: Some(if e.is_timeout() { "no answer in 10 s".into() } else { e.to_string() }),
        },
    }
}

#[cfg(test)]
mod t {
    use p256::ecdsa::{signature::Verifier, DerSignature, VerifyingKey};
    use p256::pkcs8::DecodePublicKey;

    use super::*;

    #[test]
    fn signature_verifies_with_published_key() {
        let dir = std::env::temp_dir().join(format!("stuntman-sig-{}", std::process::id()));
        let s = Signer::load_or_create(&dir.join("k.pem")).unwrap();
        let sig = s.sign("1700000000", b"[{\"event\":\"delivered\"}]");
        let vk = VerifyingKey::from_public_key_der(&B64.decode(s.public_key_b64()).unwrap()).unwrap();
        let sig = DerSignature::try_from(B64.decode(sig).unwrap().as_slice()).unwrap();
        vk.verify(b"1700000000[{\"event\":\"delivered\"}]", &sig).unwrap();
    }
}
