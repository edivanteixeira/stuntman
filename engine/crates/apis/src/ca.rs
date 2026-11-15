//! The Stuntman Test CA: made once (data/keys/test-ca.pem + test-ca.key), it signs a certificate for
//! every host name asked for over TLS (a provider's real host, <service>.<env>.stuntman.local) and the
//! client certificates apps use when a plugin requires mTLS. Trust the CA in the app's machine or
//! container and the real host names work over HTTPS against Stuntman.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rcgen::{BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

pub struct Ca {
    pub pem: String,
    cert: Certificate,
    key: KeyPair,
    leaves: Mutex<HashMap<String, Arc<rustls::sign::CertifiedKey>>>,
}

static CA: OnceLock<Ca> = OnceLock::new();
static DIR: OnceLock<PathBuf> = OnceLock::new();

/// The keys folder (data/keys), set when the APIs port starts.
pub fn set_dir(p: &Path) {
    let _ = DIR.set(p.to_path_buf());
}

fn params() -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).expect("ca params");
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Duble Test CA");
    dn.push(DnType::OrganizationName, "Duble (test only, never trust in production)");
    p.distinguished_name = dn;
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    p.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
    p.not_before = rcgen::date_time_ymd(2025, 1, 1);
    p.not_after = rcgen::date_time_ymd(2035, 1, 1);
    p
}

pub fn keys_dir() -> PathBuf {
    DIR.get().cloned().unwrap_or_else(|| PathBuf::from("data/keys"))
}

pub fn get() -> &'static Ca {
    CA.get_or_init(|| {
        let dir = DIR.get().cloned().unwrap_or_else(|| PathBuf::from("data/keys"));
        let _ = std::fs::create_dir_all(&dir);
        let (kp, cp) = (dir.join("test-ca.key"), dir.join("test-ca.pem"));
        let key = std::fs::read_to_string(&kp).ok().and_then(|t| KeyPair::from_pem(&t).ok());
        let (key, pem) = match (key, std::fs::read_to_string(&cp).ok()) {
            (Some(k), Some(pem)) => (k, pem),
            _ => {
                let k = KeyPair::generate().expect("ca key");
                let c = params().self_signed(&k).expect("ca cert");
                let pem = c.pem();
                let _ = std::fs::write(&kp, k.serialize_pem());
                let _ = std::fs::write(&cp, &pem);
                (k, pem)
            }
        };
        // The same name and key sign the leaves, so they chain to the CA saved on disk.
        let cert = params().self_signed(&key).expect("ca cert");
        Ca { pem, cert, key, leaves: Mutex::new(HashMap::new()) }
    })
}

pub fn der_of(pem: &str) -> Vec<CertificateDer<'static>> {
    pem.split("-----BEGIN CERTIFICATE-----").skip(1).filter_map(|b| b.split("-----END CERTIFICATE-----").next())
        .filter_map(|b| B64.decode(b.chars().filter(|c| !c.is_whitespace()).collect::<String>()).ok()).map(CertificateDer::from).collect()
}

impl Ca {
    /// A server certificate for this host name, signed by the CA (cached).
    pub fn leaf(&self, host: &str) -> Option<Arc<rustls::sign::CertifiedKey>> {
        let host = host.to_lowercase();
        if let Some(c) = self.leaves.lock().unwrap().get(&host) {
            return Some(c.clone());
        }
        let mut p = CertificateParams::new(vec![host.clone()]).ok()?;
        p.distinguished_name.push(DnType::CommonName, host.clone());
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        p.use_authority_key_identifier_extension = true;
        let k = KeyPair::generate().ok()?;
        let c = p.signed_by(&k, &self.cert, &self.key).ok()?;
        let key = rustls::crypto::ring::sign::any_supported_type(&PrivateKeyDer::try_from(k.serialize_der()).ok()?).ok()?;
        let ck = Arc::new(rustls::sign::CertifiedKey::new(vec![c.der().clone(), der_of(&self.pem).into_iter().next()?], key));
        self.leaves.lock().unwrap().insert(host, ck.clone());
        Some(ck)
    }

    /// A client certificate for an app (mTLS): (certificate PEM, key PEM).
    pub fn client(&self, name: &str) -> Result<(String, String), String> {
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        p.distinguished_name.push(DnType::CommonName, name);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        p.use_authority_key_identifier_extension = true;
        let k = KeyPair::generate().map_err(|e| e.to_string())?;
        let c = p.signed_by(&k, &self.cert, &self.key).map_err(|e| e.to_string())?;
        Ok((c.pem(), k.serialize_pem()))
    }

    /// A certificate for a vault: the subject's own key (PKCS#8 DER, e.g. RSA), valid from
    /// `from_days` (negative = in the past) for `days`, signed by this CA or, with `untrusted`, by a
    /// throwaway "Unknown Test CA". Returns the certificate DER.
    pub fn issue(&self, subject_cn: &str, sans: &[String], key_pkcs8: &[u8], from_days: i64, days: i64, untrusted: bool) -> Result<Vec<u8>, String> {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        self.issue_between(subject_cn, sans, key_pkcs8, now + from_days * 86_400, now + (from_days + days) * 86_400, untrusted)
    }

    /// As issue, with the validity window in Unix seconds (an expired or not-yet-valid one too).
    pub fn issue_between(&self, subject_cn: &str, sans: &[String], key_pkcs8: &[u8], not_before: i64, not_after: i64, untrusted: bool) -> Result<Vec<u8>, String> {
        let alg = &rcgen::PKCS_RSA_SHA256;
        let subject = KeyPair::from_pkcs8_der_and_sign_algo(&rustls::pki_types::PrivatePkcs8KeyDer::from(key_pkcs8.to_vec()), alg).map_err(|e| e.to_string())?;
        let mut p = CertificateParams::new(sans.to_vec()).map_err(|e| e.to_string())?;
        p.distinguished_name.push(DnType::CommonName, subject_cn);
        p.not_before = time_of(not_before);
        p.not_after = time_of(not_after);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth, ExtendedKeyUsagePurpose::ClientAuth];
        p.use_authority_key_identifier_extension = true;
        let c = if untrusted {
            let k = KeyPair::generate().map_err(|e| e.to_string())?;
            let mut cp = params();
            cp.distinguished_name = DistinguishedName::new();
            cp.distinguished_name.push(DnType::CommonName, "Unknown Test CA");
            let other = cp.self_signed(&k).map_err(|e| e.to_string())?;
            p.signed_by(&subject, &other, &k)
        } else {
            p.signed_by(&subject, &self.cert, &self.key)
        }
        .map_err(|e| e.to_string())?;
        Ok(c.der().to_vec())
    }

    /// An RSA certificate in the ICP-Brasil style (A18): the holder's name as CN and, as SAN otherNames,
    /// the CNPJ (2.16.76.1.3.3) of an e-CNPJ or the CPF (in 2.16.76.1.3.1) of an e-CPF. Signed by the
    /// Stuntman Test CA (or, with `untrusted`, by an unknown one). Returns the key PEM (PKCS#8) and the
    /// certificate DER.
    pub fn icp(&self, holder: &str, cnpj: Option<&str>, cpf: Option<&str>, not_before: i64, not_after: i64, untrusted: bool) -> Result<(String, Vec<u8>), String> {
        use rsa::pkcs8::EncodePrivateKey;
        let k = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).map_err(|e| e.to_string())?;
        let der = k.to_pkcs8_der().map_err(|e| e.to_string())?;
        let pem = k.to_pkcs8_pem(rsa::pkcs8::LineEnding::LF).map_err(|e| e.to_string())?.to_string();
        let subject = KeyPair::from_pkcs8_der_and_sign_algo(&rustls::pki_types::PrivatePkcs8KeyDer::from(der.as_bytes().to_vec()), &rcgen::PKCS_RSA_SHA256).map_err(|e| e.to_string())?;
        let mut p = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        let cn = match (cnpj, cpf) {
            (Some(c), _) => format!("{holder}:{}", c.chars().filter(|x| x.is_ascii_digit()).collect::<String>()),
            (None, Some(c)) => format!("{holder}:{}", c.chars().filter(|x| x.is_ascii_digit()).collect::<String>()),
            _ => holder.to_string(),
        };
        p.distinguished_name.push(DnType::CommonName, cn);
        p.distinguished_name.push(DnType::OrganizationName, "ICP-Brasil (Stuntman test)");
        p.distinguished_name.push(DnType::CountryName, "BR");
        if let Some(c) = cnpj {
            let d: String = c.chars().filter(|x| x.is_ascii_digit()).collect();
            p.subject_alt_names.push(rcgen::SanType::OtherName((vec![2, 16, 76, 1, 3, 3], rcgen::OtherNameValue::Utf8String(d))));
            p.subject_alt_names.push(rcgen::SanType::OtherName((vec![2, 16, 76, 1, 3, 2], rcgen::OtherNameValue::Utf8String(holder.to_uppercase()))));
        }
        if let Some(c) = cpf {
            let d: String = c.chars().filter(|x| x.is_ascii_digit()).collect();
            p.subject_alt_names.push(rcgen::SanType::OtherName((vec![2, 16, 76, 1, 3, 1], rcgen::OtherNameValue::Utf8String(format!("01011990{d}00000000000000000000000000")))));
        }
        p.not_before = time_of(not_before);
        p.not_after = time_of(not_after);
        p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth, ExtendedKeyUsagePurpose::EmailProtection];
        p.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature, rcgen::KeyUsagePurpose::ContentCommitment, rcgen::KeyUsagePurpose::KeyEncipherment];
        p.use_authority_key_identifier_extension = true;
        let c = if untrusted {
            let ok = KeyPair::generate().map_err(|e| e.to_string())?;
            let mut cp = params();
            cp.distinguished_name = DistinguishedName::new();
            cp.distinguished_name.push(DnType::CommonName, "Unknown Test CA");
            let other = cp.self_signed(&ok).map_err(|e| e.to_string())?;
            p.signed_by(&subject, &other, &ok)
        } else {
            p.signed_by(&subject, &self.cert, &self.key)
        }
        .map_err(|e| e.to_string())?;
        Ok((pem, c.der().to_vec()))
    }

    pub fn roots(&self) -> rustls::RootCertStore {
        let mut r = rustls::RootCertStore::empty();
        for d in der_of(&self.pem) {
            let _ = r.add(d);
        }
        r
    }
}

fn time_of(secs: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}
