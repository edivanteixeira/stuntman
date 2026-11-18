//! HTTPS for the APIs (default port 8443): the ClientHello's host name (SNI) picks the environment
//! and the plugin, Stuntman presents a certificate for that name signed by the Stuntman Test CA, and a
//! plugin that requires a client certificate gets real mTLS: no certificate, or one the CA did not
//! sign, fails the TLS handshake like the provider would. Refused handshakes are listed on Calls.

use std::sync::Arc;

use axum::{body::Body, Router};
use stuntman_core::{env::Envs, netserve::{Kill, Remote}, util};
use hyper_util::rt::TokioIo;
use serde_json::json;
use sha2::Digest;
use tokio::net::TcpListener;

use crate::{ca, engine, plugin, store};

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// The app that presented a client certificate (its CN when Stuntman issued it, else a fingerprint).
#[derive(Clone)]
pub struct ClientCert(pub String);

/// The request came over the HTTPS port (request.scheme in WireMock templates).
#[derive(Clone, Copy)]
pub struct Tls;

/// The client certificate itself (DER), for the CNPJ of an ICP-Brasil style one.
#[derive(Clone)]
pub struct ClientCertDer(pub Vec<u8>);

pub fn fingerprint(der: &[u8]) -> String {
    hex::encode(&sha2::Sha256::digest(der)[..8])
}

fn client_names() -> std::path::PathBuf {
    ca::keys_dir().join("client-certs.json")
}

pub fn remember_client(der_pem: &str, name: &str) {
    let mut m: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(client_names()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    for d in ca::der_of(der_pem) {
        m.insert(fingerprint(&d), json!(name));
    }
    let _ = std::fs::write(client_names(), serde_json::to_string_pretty(&m).unwrap_or_default());
}

fn client_name(der: &[u8]) -> String {
    let f = fingerprint(der);
    std::fs::read_to_string(client_names()).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok()).and_then(|m| m[&f].as_str().map(String::from)).unwrap_or(format!("cert {f}"))
}

pub async fn serve(port: u16, envs: Envs, app: Router) {
    let l = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("APIs HTTPS: port {port}: {e}");
            *crate::TLS_BIND_ERROR.lock().unwrap() = Some(format!("port {port}: {e}"));
            return;
        }
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let (app, envs, provider) = (app.clone(), envs.clone(), provider.clone());
        tokio::spawn(async move {
            let Ok(std) = sock.into_std() else { return };
            let dup = std.try_clone().ok();
            let Ok(sock) = tokio::net::TcpStream::from_std(std) else { return };
            let Ok(start) = tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), sock).await else { return };
            let sni = start.client_hello().server_name().unwrap_or("localhost").to_string();
            let ctx = envs.by_host(Some(&sni));
            let plugins = ctx.as_ref().map(plugin::all).unwrap_or_default();
            let svc = engine::find_service(&plugins, &sni, "/").map(|(p, _)| p.clone());
            let mtls = svc.as_ref().map(|p| p.service.auth.client_certificate.as_ref().map(|c| c.required).unwrap_or(false)).unwrap_or(false);
            let Some(leaf) = ca::get().leaf(&sni) else { return };
            let b = rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions().expect("tls versions");
            let b = if mtls {
                match rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(ca::get().roots()), provider.clone()).build() {
                    Ok(v) => b.with_client_cert_verifier(v),
                    Err(_) => return,
                }
            } else {
                b.with_no_client_auth()
            };
            let mut cfg = b.with_cert_resolver(Arc::new(Fixed(leaf)));
            cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
            let tls = match start.into_stream(Arc::new(cfg)).await {
                Ok(t) => t,
                Err(e) => {
                    // The handshake failed: no client certificate, an untrusted one, or the app does
                    // not trust the Stuntman Test CA. Listed so the screen says which.
                    if let (Some(ctx), Some(p)) = (ctx, svc) {
                        let msg = e.to_string();
                        let why = if msg.contains("UnknownCA") || msg.contains("BadCertificate") || msg.contains("unknown ca") || msg.contains("CertificateUnknown") {
                            if mtls && !msg.to_lowercase().contains("alert") { "client certificate refused (not signed by the Stuntman Test CA)".to_string() } else { "the app does not trust the Stuntman Test CA yet".to_string() }
                        } else if msg.contains("NoCertificatesPresented") || msg.contains("no certificate") || msg.contains("PeerIncompatible") {
                            "no client certificate: the provider requires one (mTLS)".to_string()
                        } else {
                            format!("TLS handshake failed: {msg}")
                        };
                        let _ = store::record(&ctx, store::Call {
                            id: util::id("call"), service: p.service.name.clone(), route: String::new(), method: "TLS".into(), path: sni.clone(), status: 0, answer: "tls_refused".into(),
                            sentence: format!("TLS handshake refused for {sni}: {why}"), billable: false, caller: addr.ip().to_string(), trace_id: None, ms: 0, provoked: Some(why), sent: json!({}),
                            answered: json!({}), request: json!({"sni": sni}), response: json!({}), steps: json!([]),
                        }).await;
                    }
                    return;
                }
            };
            let der = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.as_ref().to_vec());
            let cert = der.as_ref().map(|d| client_name(d));
            // ICP-Brasil style: a certificate without a CNPJ is refused, as SEFAZ does.
            let icp = svc.as_ref().map(|p| p.service.auth.client_certificate.as_ref().map(|c| c.icp).unwrap_or(false)).unwrap_or(false);
            if icp && der.as_ref().and_then(|d| ca::cnpj_of(d)).is_none() {
                if let (Some(ctx), Some(p)) = (&ctx, &svc) {
                    let _ = store::record(ctx, store::Call {
                        id: util::id("call"), service: p.service.name.clone(), route: String::new(), method: "TLS".into(), path: sni.clone(), status: 0, answer: "tls_refused".into(),
                        sentence: format!("TLS refused for {sni}: the client certificate has no CNPJ (ICP-Brasil e-CNPJ required)"), billable: false, caller: addr.ip().to_string(), trace_id: None, ms: 0,
                        provoked: Some("no CNPJ in the certificate".into()), sent: json!({}), answered: json!({}), request: json!({"sni": sni}), response: json!({}), steps: json!([]),
                    }).await;
                }
                return;
            }
            let kill = Kill::new();
            let k2 = kill.clone();
            let svc_fn = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(k2.clone());
                req.extensions_mut().insert(Remote(addr));
                req.extensions_mut().insert(Tls);
                if let Some(c) = &cert {
                    req.extensions_mut().insert(ClientCert(c.clone()));
                }
                if let Some(d) = &der {
                    req.extensions_mut().insert(ClientCertDer(d.clone()));
                }
                let app = app.clone();
                async move {
                    use tower::ServiceExt;
                    app.oneshot(req.map(Body::new)).await
                }
            });
            let conn = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc_fn);
            tokio::select! {
                _ = conn => {}
                r = kill.wait() => {
                    if r {
                        if let Some(d) = &dup {
                            let _ = socket2::SockRef::from(d).set_linger(Some(std::time::Duration::ZERO));
                        }
                    }
                }
            }
            drop(dup);
        });
    }
}
