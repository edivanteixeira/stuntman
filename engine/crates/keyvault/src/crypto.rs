//! Real cryptography: keys never leave Stuntman, as in a vault. RSA keys (sign/verify RS256-512,
//! PS256-512; wrap/unwrap and encrypt/decrypt RSA-OAEP, RSA-OAEP-256, RSA1_5), their public part as
//! JWK or PEM, and certificates issued by the Test CA with a .pfx (the certificate's secret).

use base64::{engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64U}, Engine};
use rsa::{
    pkcs1v15::Pkcs1v15Encrypt,
    pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey, LineEnding},
    traits::PublicKeyParts,
    Oaep, Pkcs1v15Sign, Pss, RsaPrivateKey, RsaPublicKey,
};
use sha2::{Digest, Sha256, Sha384, Sha512};

pub fn generate(bits: usize) -> Result<String, String> {
    let k = RsaPrivateKey::new(&mut rand::thread_rng(), bits).map_err(|e| e.to_string())?;
    k.to_pkcs8_pem(LineEnding::LF).map(|p| p.to_string()).map_err(|e| e.to_string())
}

pub fn private(pem: &str) -> Result<RsaPrivateKey, String> {
    RsaPrivateKey::from_pkcs8_pem(pem).map_err(|e| e.to_string())
}

pub fn pkcs8_der(pem: &str) -> Result<Vec<u8>, String> {
    Ok(private(pem)?.to_pkcs8_der().map_err(|e| e.to_string())?.as_bytes().to_vec())
}

pub fn pem_from_pkcs8_der(der: &[u8]) -> Result<String, String> {
    let k = RsaPrivateKey::from_pkcs8_der(der).map_err(|e| e.to_string())?;
    k.to_pkcs8_pem(LineEnding::LF).map(|p| p.to_string()).map_err(|e| e.to_string())
}

pub fn jwk(pem: &str, kid: &str, ops: &[String]) -> Result<serde_json::Value, String> {
    let k = private(pem)?;
    let p = RsaPublicKey::from(&k);
    Ok(serde_json::json!({"kid": kid, "kty": "RSA", "key_ops": ops, "n": B64U.encode(p.n().to_bytes_be()), "e": B64U.encode(p.e().to_bytes_be())}))
}

pub fn public_pem(pem: &str) -> Result<String, String> {
    let p = RsaPublicKey::from(&private(pem)?);
    p.to_public_key_pem(LineEnding::LF).map_err(|e| e.to_string())
}

pub fn bits(pem: &str) -> usize {
    private(pem).map(|k| k.size() * 8).unwrap_or(0)
}

fn scheme_sign(alg: &str) -> Result<(Option<Pkcs1v15Sign>, Option<Pss>), String> {
    Ok(match alg {
        "RS256" => (Some(Pkcs1v15Sign::new::<Sha256>()), None),
        "RS384" => (Some(Pkcs1v15Sign::new::<Sha384>()), None),
        "RS512" => (Some(Pkcs1v15Sign::new::<Sha512>()), None),
        "PS256" => (None, Some(Pss::new::<Sha256>())),
        "PS384" => (None, Some(Pss::new::<Sha384>())),
        "PS512" => (None, Some(Pss::new::<Sha512>())),
        _ => return Err(format!("Algorithm {alg} is not supported for RSA keys (RS256, RS384, RS512, PS256, PS384, PS512)")),
    })
}

/// Signs a digest (as the Key Vault sign operation: the app hashes, the vault signs).
pub fn sign(pem: &str, alg: &str, digest: &[u8]) -> Result<Vec<u8>, String> {
    let k = private(pem)?;
    match scheme_sign(alg)? {
        (Some(s), _) => k.sign(s, digest).map_err(|e| e.to_string()),
        (_, Some(s)) => k.sign_with_rng(&mut rand::thread_rng(), s, digest).map_err(|e| e.to_string()),
        _ => unreachable!(),
    }
}

pub fn verify(pem: &str, alg: &str, digest: &[u8], sig: &[u8]) -> Result<bool, String> {
    let p = RsaPublicKey::from(&private(pem)?);
    Ok(match scheme_sign(alg)? {
        (Some(s), _) => p.verify(s, digest, sig).is_ok(),
        (_, Some(s)) => p.verify(s, digest, sig).is_ok(),
        _ => false,
    })
}

pub fn encrypt(pem: &str, alg: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let p = RsaPublicKey::from(&private(pem)?);
    let mut r = rand::thread_rng();
    match alg {
        "RSA-OAEP" => p.encrypt(&mut r, Oaep::new::<sha1::Sha1>(), data),
        "RSA-OAEP-256" => p.encrypt(&mut r, Oaep::new::<Sha256>(), data),
        "RSA1_5" => p.encrypt(&mut r, Pkcs1v15Encrypt, data),
        _ => return Err(format!("Algorithm {alg} is not supported (RSA-OAEP, RSA-OAEP-256, RSA1_5)")),
    }
    .map_err(|e| e.to_string())
}

pub fn decrypt(pem: &str, alg: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let k = private(pem)?;
    match alg {
        "RSA-OAEP" => k.decrypt(Oaep::new::<sha1::Sha1>(), data),
        "RSA-OAEP-256" => k.decrypt(Oaep::new::<Sha256>(), data),
        "RSA1_5" => k.decrypt(Pkcs1v15Encrypt, data),
        _ => return Err(format!("Algorithm {alg} is not supported (RSA-OAEP, RSA-OAEP-256, RSA1_5)")),
    }
    .map_err(|_| "The ciphertext was wrapped with another key or was changed".to_string())
}

pub fn b64u(d: &[u8]) -> String {
    B64U.encode(d)
}
pub fn unb64u(s: &str) -> Result<Vec<u8>, String> {
    B64U.decode(s.trim_end_matches('=')).or_else(|_| B64.decode(s)).map_err(|_| "value is not base64url".to_string())
}
pub fn b64(d: &[u8]) -> String {
    B64.encode(d)
}
pub fn unb64(s: &str) -> Result<Vec<u8>, String> {
    B64.decode(s.trim()).or_else(|_| B64U.decode(s.trim().trim_end_matches('='))).map_err(|_| "value is not base64".to_string())
}

pub fn thumbprint_sha1(der: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    sha1::Sha1::digest(der).to_vec()
}

/// A .pfx with the key and the certificate (empty password, as Key Vault gives it).
pub fn pfx(key_pem: &str, cert_der: &[u8], password: &str) -> Result<Vec<u8>, String> {
    use p12_keystore::{Certificate, KeyStore, KeyStoreEntry, PrivateKey, PrivateKeyChain};
    let cert = Certificate::from_der(cert_der).map_err(|e| e.to_string())?;
    let key = PrivateKey::from_der(&pkcs8_der(key_pem)?).map_err(|e| e.to_string())?;
    let mut ks = KeyStore::new();
    let id = Sha256::digest(cert_der)[..20].to_vec();
    ks.add_entry("certificate", KeyStoreEntry::PrivateKeyChain(PrivateKeyChain::new(id, key, [cert])));
    ks.writer(password).write().map_err(|e| e.to_string())
}

/// (key PEM, certificate DER) from an imported .pfx or PEM bundle.
pub fn import(value: &str, password: &str) -> Result<(String, Vec<u8>), String> {
    if value.contains("-----BEGIN") {
        let key = value.split("-----BEGIN PRIVATE KEY-----").nth(1).and_then(|r| r.split("-----END PRIVATE KEY-----").next())
            .ok_or("The PEM has no PRIVATE KEY (PKCS#8) block")?;
        let key = pem_from_pkcs8_der(&unb64(&key.chars().filter(|c| !c.is_whitespace()).collect::<String>())?)?;
        let cert = value.split("-----BEGIN CERTIFICATE-----").nth(1).and_then(|r| r.split("-----END CERTIFICATE-----").next()).ok_or("The PEM has no CERTIFICATE block")?;
        return Ok((key, unb64(&cert.chars().filter(|c| !c.is_whitespace()).collect::<String>())?));
    }
    let data = unb64(value)?;
    let ks = p12_keystore::KeyStore::from_pkcs12(&data, password, p12_keystore::Pkcs12ImportPolicy::Relaxed).map_err(|_| "The .pfx could not be opened with that password".to_string())?;
    let (_, chain) = ks.private_key_chain().ok_or("The .pfx has no private key")?;
    let cert = chain.certs().first().ok_or("The .pfx has no certificate")?;
    Ok((pem_from_pkcs8_der(chain.key().as_der())?, cert.as_der().to_vec()))
}

/// Subject, issuer, validity (unix seconds) of a certificate.
pub fn cert_info(der: &[u8]) -> (String, String, i64, i64) {
    match x509_parser::parse_x509_certificate(der) {
        Ok((_, c)) => (c.subject().to_string(), c.issuer().to_string(), c.validity().not_before.timestamp(), c.validity().not_after.timestamp()),
        Err(_) => (String::new(), String::new(), 0, 0),
    }
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn rsa_round_trips() {
        let pem = generate(1024).unwrap();
        let d = Sha256::digest(b"document 58213").to_vec();
        for alg in ["RS256", "PS256"] {
            let s = sign(&pem, alg, &d).unwrap();
            assert!(verify(&pem, alg, &d, &s).unwrap());
        }
        let k = b"0123456789abcdef0123456789abcdef";
        for alg in ["RSA-OAEP", "RSA-OAEP-256", "RSA1_5"] {
            let w = encrypt(&pem, alg, k).unwrap();
            assert_eq!(decrypt(&pem, alg, &w).unwrap(), k);
        }
        let other = generate(1024).unwrap();
        assert!(decrypt(&other, "RSA-OAEP", &encrypt(&pem, "RSA-OAEP", k).unwrap()).is_err());
        assert_eq!(jwk(&pem, "kid", &[]).unwrap()["e"], "AQAB");
    }
}
