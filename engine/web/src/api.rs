//! Talks to the same HTTP API that tests and agents use; nothing here is private to the screen.

use gloo_net::http::{Request, RequestBuilder};
use leptos::prelude::*;
use serde_json::Value;
use wasm_bindgen::{closure::Closure, JsCast};

thread_local! {
    /// "/e/<name>" when the screen works on an environment chosen in the switcher, else "".
    static PREFIX: std::cell::RefCell<String> = std::cell::RefCell::new(stored_env().map(|n| format!("/e/{n}")).unwrap_or_default());
}

const ENV_KEY: &str = "stuntman.env";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok()?
}

/// The environment picked in the switcher, kept per browser.
pub fn stored_env() -> Option<String> {
    storage()?.get_item(ENV_KEY).ok()?.filter(|s| !s.is_empty())
}

/// Opens another environment: every screen, the live stream and the rail start over on it.
pub fn switch_env(name: &str, path: &str) {
    if let Some(s) = storage() {
        let _ = if name.is_empty() { s.remove_item(ENV_KEY) } else { s.set_item(ENV_KEY, name) };
    }
    if let Some(w) = web_sys::window() {
        let _ = w.location().assign(path);
    }
}

/// A server path in the current environment: url("/api/messages/x/raw").
pub fn url(path: &str) -> String {
    PREFIX.with(|p| format!("{}{path}", p.borrow()))
}

fn with_client(r: RequestBuilder) -> RequestBuilder {
    r.header("X-Stuntman-Client", "ui")
}

async fn finish(r: Result<gloo_net::http::Response, gloo_net::Error>) -> Result<Value, String> {
    let r = r.map_err(|e| format!("Stuntman is not answering: {e}"))?;
    let ok = r.ok();
    let v: Value = r.json().await.unwrap_or(Value::Null);
    if ok {
        Ok(v)
    } else {
        Err(v["error"].as_str().unwrap_or("Something went wrong").to_string())
    }
}

pub async fn get(path: &str) -> Result<Value, String> {
    finish(with_client(Request::get(&url(&format!("/api{path}")))).send().await).await
}

pub async fn send(method: &str, path: &str, body: Value) -> Result<Value, String> {
    let u = url(&format!("/api{path}"));
    let b = match method {
        "POST" => Request::post(&u),
        "PUT" => Request::put(&u),
        "PATCH" => Request::patch(&u),
        _ => Request::delete(&u),
    };
    let req = with_client(b).json(&body).map_err(|e| e.to_string())?;
    finish(req.send().await).await
}

pub async fn text(path: &str) -> Result<String, String> {
    let r = Request::get(&url(path)).send().await.map_err(|e| e.to_string())?;
    r.text().await.map_err(|e| e.to_string())
}

/// Encodes a value for a query string.
pub fn enc(s: &str) -> String {
    js_sys::encode_uri_component(s).as_string().unwrap_or_default()
}

pub fn s(v: &Value, k: &str) -> String {
    match v.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    }
}

pub fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(0)
}

/// Live changes from the server (SSE): a counter per change, and the last change.
#[derive(Clone, Copy)]
pub struct Live {
    pub tick: RwSignal<u64>,
    pub last: RwSignal<Value>,
    pub connected: RwSignal<bool>,
}

/// One live stream per page. It is closed when the page is hidden or cached (pagehide) and opened
/// again when it comes back: browsers allow 6 connections per host over HTTP/1.1, and open streams
/// from cached pages would otherwise use them all and stall every other request.
pub fn live() -> Live {
    let l = Live { tick: RwSignal::new(0), last: RwSignal::new(Value::Null), connected: RwSignal::new(false) };
    let current: std::rc::Rc<std::cell::RefCell<Option<web_sys::EventSource>>> = Default::default();
    let open = {
        let current = current.clone();
        move || {
            if current.borrow().is_some() {
                return;
            }
            *current.borrow_mut() = Some(open_stream(l));
        }
    };
    open();
    let w = web_sys::window().unwrap();
    let c2 = current.clone();
    let hide = Closure::<dyn FnMut()>::new(move || {
        if let Some(es) = c2.borrow_mut().take() {
            es.close();
        }
    });
    w.add_event_listener_with_callback("pagehide", hide.as_ref().unchecked_ref()).unwrap();
    hide.forget();
    let show = Closure::<dyn FnMut()>::new(open);
    w.add_event_listener_with_callback("pageshow", show.as_ref().unchecked_ref()).unwrap();
    show.forget();
    l
}

fn open_stream(l: Live) -> web_sys::EventSource {
    let es = web_sys::EventSource::new(&url("/api/stream")).expect("EventSource");
    let on_change = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| {
        if let Some(t) = e.data().as_string() {
            l.last.set(serde_json::from_str(&t).unwrap_or(Value::Null));
            l.tick.update(|n| *n += 1);
        }
    });
    es.add_event_listener_with_callback("change", on_change.as_ref().unchecked_ref()).unwrap();
    on_change.forget();
    let open = Closure::<dyn FnMut()>::new(move || l.connected.set(true));
    es.set_onopen(Some(open.as_ref().unchecked_ref()));
    open.forget();
    let err = Closure::<dyn FnMut()>::new(move || l.connected.set(false));
    es.set_onerror(Some(err.as_ref().unchecked_ref()));
    err.forget();
    es
}

pub fn now() -> f64 {
    js_sys::Date::now()
}

/// 14:32
pub fn hm(ms: i64) -> String {
    let d = js_sys::Date::new(&(ms as f64).into());
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

/// 14:32:07
pub fn hms(ms: i64) -> String {
    let d = js_sys::Date::new(&(ms as f64).into());
    format!("{:02}:{:02}:{:02}", d.get_hours(), d.get_minutes(), d.get_seconds())
}

/// "18 min", "2 h", "40 s"
pub fn span(ms: i64) -> String {
    let s = (ms.abs() / 1000).max(0);
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else if s < 86_400 {
        let h = s / 3600;
        let m = (s % 3600) / 60;
        if m == 0 || h >= 10 { format!("{h} h") } else { format!("{h} h {m} min") }
    } else {
        format!("{} days", s / 86_400)
    }
}

pub fn ago(ms: i64) -> String {
    format!("{} ago", span(now() as i64 - ms))
}

pub fn kb(n: i64) -> String {
    if n >= 1 << 20 {
        format!("{:.1} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1024 {
        format!("{} KB", (n + 512) / 1024)
    } else {
        format!("{n} B")
    }
}

/// 18278 → "18,278", as the screens write counts.
pub fn thousands(n: i64) -> String {
    let s = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}
