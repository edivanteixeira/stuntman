//! Storage screens (C3): the accounts column beside every tab; per account Objects, Rules, Links,
//! Events and Settings (Azure Blob and S3).

pub mod accounts;
pub mod area;
pub mod events;
pub mod links;
pub mod objects;
pub mod rules;

use leptos::prelude::*;
use wasm_bindgen::JsCast;


pub fn service_label(s: &str) -> &'static str {
    if s == "s3" { "S3" } else { "Azure Blob" }
}

/// S2 rules / S5 settings: the shared storage header (the service name stays for callers).
#[component]
pub fn ServiceHead(service: String, active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let _ = service;
    view! { <area::StorageHead active=if active == "accounts" { "settings" } else { active } right=right/> }
}

/// S1/S3/S4 (objects, links, events): the shared storage header with the tab's search.
#[component]
pub fn FilesHead(active: &'static str, q: RwSignal<String>, placeholder: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    view! { <area::StorageHead active=active right=move || view! {
        <input class="input" style="width:172px" placeholder=placeholder prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
        {right.run()}
    }/> }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(d: &[u8]) -> String {
    let mut o = String::with_capacity(d.len() * 4 / 3 + 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

/// The bytes of a picked or dropped file.
pub async fn read_file(f: web_sys::File) -> Result<Vec<u8>, String> {
    let buf = wasm_bindgen_futures::JsFuture::from(f.array_buffer()).await.map_err(|_| "could not read the file".to_string())?;
    Ok(js_sys::Uint8Array::new(&buf).to_vec())
}

pub fn files_of_input(e: &leptos::ev::Event) -> Vec<web_sys::File> {
    let Some(i) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) else { return vec![] };
    let mut out = vec![];
    if let Some(l) = i.files() {
        for k in 0..l.length() {
            if let Some(f) = l.get(k) {
                out.push(f);
            }
        }
    }
    i.set_value("");
    out
}

pub fn files_of_drop(e: &web_sys::DragEvent) -> Vec<web_sys::File> {
    let mut out = vec![];
    if let Some(l) = e.data_transfer().and_then(|d| d.files()) {
        for k in 0..l.length() {
            if let Some(f) = l.get(k) {
                out.push(f);
            }
        }
    }
    out
}

/// "403 Forbidden" for a next-read value.
pub fn next_label(r: &str) -> String {
    match r {
        "403" => "403 Forbidden".into(),
        "404" => "404 Not found".into(),
        "503" => "503 Server busy".into(),
        "truncate" => "Cut off halfway".into(),
        "bad_md5" => "Wrong checksum".into(),
        s if s.starts_with("slow") => format!("Slow · {}, then the file", s.trim_start_matches("slow:")),
        s => s.to_string(),
    }
}

pub fn who(s: &str) -> String {
    s.trim_start_matches("api:").trim_start_matches("mcp:").to_string()
}
