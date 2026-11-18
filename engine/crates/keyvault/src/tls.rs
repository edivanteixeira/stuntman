//! The vaults' HTTPS port (default 8444): Azure SDKs refuse Bearer tokens over plain HTTP, so each
//! vault answers at https://<vault>.vault.<environment domain>:<port> with a certificate for that
//! name signed by the Stuntman Test CA (trust test-ca.pem in the app's test machine or container).

use std::sync::{Arc, Mutex};

use http_body_util::BodyExt;
use hyper_util::rt::TokioIo;
use stuntman_core::env::Envs;
use tokio::net::TcpListener;

pub static BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

pub async fn serve(port: u16, envs: Envs) {
    let l = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Key Vault HTTPS: port {port}: {e}");
            *BIND_ERROR.lock().unwrap() = Some(format!("port {port}: {e}"));
            return;
        }
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    loop {
        let Ok((sock, _addr)) = l.accept().await else { continue };
        let (envs, provider) = (envs.clone(), provider.clone());
        tokio::spawn(async move {
            let Ok(start) = tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), sock).await else { return };
            let sni = start.client_hello().server_name().unwrap_or("localhost").to_string();
            let Some(leaf) = stuntman_apis::ca::get().leaf(&sni) else { return };
            let Ok(b) = rustls::ServerConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions() else { return };
            let mut cfg = b.with_no_client_auth().with_cert_resolver(Arc::new(Fixed(leaf)));
            cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
            let Ok(tls) = start.into_stream(Arc::new(cfg)).await else { return };
            let svc = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let (envs, sni) = (envs.clone(), sni.clone());
                async move {
                    let (parts, body) = req.into_parts();
                    let body = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
                    let host = parts.headers.get("host").and_then(|h| h.to_str().ok()).unwrap_or(&sni).to_string();
                    let resp = match envs.by_host(Some(&host)).or_else(|| envs.by_host(Some(&sni))) {
                        Some(ctx) => {
                            // set_vault reachability=refused: the connection closes with no answer.
                            if crate::azure::reachability(&ctx, host.split('.').next().unwrap_or("")).await.as_deref() == Some("refused") {
                                return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "vault unreachable (provoked)"));
                            }
                            crate::azure::handle(ctx, host, parts.method, parts.uri.path().to_string(), parts.uri.query().unwrap_or("").to_string(), parts.headers, body).await
                        }
                        None => axum::response::IntoResponse::into_response((axum::http::StatusCode::NOT_FOUND, "no environment for this host")),
                    };
                    Ok::<_, std::io::Error>(resp)
                }
            });
            let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
        });
    }
}
