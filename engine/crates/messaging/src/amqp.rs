//! One AMQP port for both protocols: the first 8 bytes say which. "AMQP 0 0 9 1" is RabbitMQ;
//! "AMQP 3 1 0 0" (SASL) or "AMQP 0 1 0 0" is AMQP 1.0, what Azure Service Bus speaks; anything
//! else is Service Bus management over HTTP. The TLS port (amqps, 5671) answers the same three
//! behind a certificate for the name asked, signed by the Stuntman Test CA. The management port
//! (5300, like the official emulator) answers all of them, with or without TLS.

use std::sync::{Arc, Mutex};

use stuntman_core::env::Envs;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    net::TcpListener,
};

pub trait Stream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Stream for T {}
pub type Io = Box<dyn Stream>;

pub static BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);
pub static TLS_BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);
pub static MGMT_BIND_ERROR: Mutex<Option<String>> = Mutex::new(None);

async fn bind(port: u16, err: &'static Mutex<Option<String>>, what: &str) -> Option<TcpListener> {
    // [::] takes IPv6 and, on macOS and Linux by default, IPv4 too: *.localhost resolves to ::1.
    match TcpListener::bind(("::", port)).await {
        Ok(l) => Some(l),
        Err(_) => match TcpListener::bind(("0.0.0.0", port)).await {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("{what}: port {port}: {e}");
                *err.lock().unwrap() = Some(format!("port {port}: {e}"));
                None
            }
        },
    }
}

/// Reads the first 8 bytes and hands the connection to the protocol that wrote them.
pub async fn dispatch(envs: Envs, mut io: Io, remote: String) {
    let mut h = [0u8; 8];
    if io.read_exact(&mut h).await.is_err() {
        return;
    }
    if &h[..4] != b"AMQP" {
        crate::mgmt::serve(envs, io, h).await;
        return;
    }
    match (h[4], h[5], h[6], h[7]) {
        (0, 0, 9, 1) => crate::rabbit::serve(envs, io, remote).await,
        _ => crate::amqp1::serve(envs, io, remote, h).await,
    }
}

pub async fn serve(envs: Envs, port: u16) {
    let Some(l) = bind(port, &BIND_ERROR, "AMQP").await else { return };
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let _ = sock.set_nodelay(true);
        tokio::spawn(dispatch(envs.clone(), Box::new(sock), addr.to_string()));
    }
}

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// amqps: the SDKs without the emulator flag (and Python's admin client, which insists on HTTPS).
pub async fn serve_tls(envs: Envs, port: u16) {
    let Some(l) = bind(port, &TLS_BIND_ERROR, "AMQPS").await else { return };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let _ = sock.set_nodelay(true);
        tokio::spawn(tls(envs.clone(), sock, addr.to_string(), provider.clone()));
    }
}

/// The TLS handshake with a leaf for the name asked, then the same dispatch as plain AMQP.
async fn tls(envs: Envs, sock: tokio::net::TcpStream, remote: String, provider: Arc<rustls::crypto::CryptoProvider>) {
    let Ok(start) = tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), sock).await else { return };
    let sni = start.client_hello().server_name().unwrap_or("localhost").to_string();
    let Some(leaf) = stuntman_apis::ca::get().leaf(&sni) else { return };
    let Ok(b) = rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions() else { return };
    let mut cfg = b.with_no_client_auth().with_cert_resolver(Arc::new(Fixed(leaf)));
    cfg.alpn_protocols = vec![b"amqp".to_vec(), b"http/1.1".to_vec()];
    let Ok(tls) = start.into_stream(Arc::new(cfg)).await else { return };
    dispatch(envs, Box::new(tls), remote).await;
}

/// The Service Bus management port (5300, as the official emulator): what an administration
/// client calls when the connection string says `sb://host:5300;UseDevelopmentEmulator=true`
/// (.NET and JS: plain HTTP on the string's port, 80 without one) or HTTPS on it (Python, Java).
/// It answers plain HTTP, HTTPS (a TLS ClientHello starts with 0x16) and AMQP too, so one
/// connection string with this port works for the admin client and the messaging client alike.
pub async fn serve_mgmt(envs: Envs, port: u16) {
    let Some(l) = bind(port, &MGMT_BIND_ERROR, "Service Bus management").await else { return };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let _ = sock.set_nodelay(true);
        let (envs, provider) = (envs.clone(), provider.clone());
        tokio::spawn(async move {
            let mut b = [0u8; 1];
            if sock.peek(&mut b).await.unwrap_or(0) == 0 {
                return;
            }
            if b[0] == 0x16 {
                tls(envs, sock, addr.to_string(), provider).await;
            } else {
                dispatch(envs, Box::new(sock), addr.to_string()).await;
            }
        });
    }
}
