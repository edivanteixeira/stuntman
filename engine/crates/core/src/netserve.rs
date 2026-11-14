//! The provider ports (Azure Blob, S3). Served connection by connection so a handler can end the
//! socket for real: "empty response" closes it without a byte, "connection reset" sends a RST.
//! Each request finds its environment by the /e/<name>/ prefix or the host name.

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use crate::{env::Envs, Ctx};
use hyper_util::rt::TokioIo;
use tokio::{net::TcpListener, sync::Notify};

/// Ends the connection this request came on.
#[derive(Clone)]
pub struct Kill {
    n: Arc<Notify>,
    reset: Arc<AtomicBool>,
}

impl Kill {
    pub fn new() -> Self {
        Kill { n: Arc::new(Notify::new()), reset: Arc::new(AtomicBool::new(false)) }
    }

    /// Waits until fired; true means a TCP reset was asked for.
    pub async fn wait(&self) -> bool {
        self.n.notified().await;
        self.reset.load(Ordering::Relaxed)
    }

    /// Close without answering (empty reply), or with a TCP reset.
    pub fn fire(&self, reset: bool) {
        self.reset.store(reset, Ordering::Relaxed);
        self.n.notify_one();
    }
}

#[derive(Clone)]
pub struct Remote(pub SocketAddr);

/// The environment's context, and the request with the /e/<name> prefix taken off (the prefix is
/// kept apart: Azure's SharedKey signs the path as sent).
pub struct Routed {
    pub ctx: Ctx,
    pub prefix: String,
}

pub fn route(envs: &Envs, req: &mut Request) -> Result<Routed, Response> {
    let path = req.uri().path().to_string();
    if let Some(r) = path.strip_prefix("/e/") {
        let (name, rest) = r.split_once('/').unwrap_or((r, ""));
        let ctx = envs.get(name).ok_or_else(|| (StatusCode::NOT_FOUND, format!("No Stuntman environment {name}")).into_response())?;
        let q = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        *req.uri_mut() = format!("/{rest}{q}").parse().map_err(|_| StatusCode::BAD_REQUEST.into_response())?;
        return Ok(Routed { ctx, prefix: format!("/e/{name}") });
    }
    let host = req.headers().get("host").and_then(|v| v.to_str().ok());
    envs.by_host(host).map(|ctx| Routed { ctx, prefix: String::new() }).ok_or_else(|| (StatusCode::NOT_FOUND, "No Stuntman environment answers for this host").into_response())
}

pub fn lower_headers(h: &HeaderMap) -> std::collections::BTreeMap<String, String> {
    let mut m = std::collections::BTreeMap::new();
    for (k, v) in h {
        let v = v.to_str().unwrap_or("").trim().to_string();
        m.entry(k.as_str().to_lowercase()).and_modify(|x: &mut String| { x.push(','); x.push_str(&v) }).or_insert(v);
    }
    m
}

/// Serves `app` on a port, one hyper connection per socket, each with its Kill switch.
pub async fn serve(port: u16, app: axum::Router, what: &'static str, bind_error: &'static std::sync::Mutex<Option<String>>) {
    let l = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{what}: port {port}: {e}");
            *bind_error.lock().unwrap() = Some(format!("port {port}: {e}"));
            return;
        }
    };
    loop {
        let Ok((sock, addr)) = l.accept().await else { continue };
        let app = app.clone();
        tokio::spawn(async move {
            let kill = Kill { n: Arc::new(Notify::new()), reset: Arc::new(AtomicBool::new(false)) };
            let k2 = kill.clone();
            let std = match sock.into_std() {
                Ok(s) => s,
                Err(_) => return,
            };
            let dup = std.try_clone().ok();
            let Ok(sock) = tokio::net::TcpStream::from_std(std) else { return };
            let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(k2.clone());
                req.extensions_mut().insert(Remote(addr));
                let app = app.clone();
                async move {
                    use tower::ServiceExt;
                    app.oneshot(req.map(Body::new)).await
                }
            });
            let conn = hyper::server::conn::http1::Builder::new().keep_alive(true).serve_connection(TokioIo::new(sock), svc);
            tokio::select! {
                _ = conn => {}
                _ = kill.n.notified() => {
                    if kill.reset.load(Ordering::Relaxed) {
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

/// A body that sends the first half and then breaks the connection (Content-Length promises all).
pub fn cut_body(data: Vec<u8>) -> Body {
    let half = data.len() / 2;
    let first = bytes::Bytes::from(data[..half].to_vec());
    // The first half is flushed before the connection breaks, so the client sees a short body.
    let s = futures::stream::unfold(Some(first), |st| async move {
        match st {
            Some(b) => Some((Ok::<_, std::io::Error>(b), None)),
            None => {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                None::<(Result<bytes::Bytes, std::io::Error>, Option<bytes::Bytes>)>
            }
        }
    });
    Body::from_stream(s)
}
