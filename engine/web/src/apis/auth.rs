//! A5 · Services › <plugin> › Auth and limits: base URL and the real host, OAuth2 client
//! credentials, billing counter with a warning, latency, rate limit, fail a share. The form edits
//! the service block of the plugin file, shown beside it.

use leptos::prelude::*;
use wasm_bindgen::JsCast;
use leptos_router::hooks::use_params_map;
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui::{self, Toggle},
};

#[component]
pub fn Auth() -> impl IntoView {
    let p = use_params_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let elsewhere = RwSignal::new(false);
    let load = move |keep: bool| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/plugins/{}/service", enc(&sv))).await {
                let dirty = draft.get_untracked() != saved.get_untracked();
                if keep && dirty && v["version"] != saved.get_untracked()["version"] { elsewhere.set(true); return; }
                if !(keep && dirty) { draft.set(v.clone()); }
                saved.set(v);
            }
        });
    };
    Effect::new(move |_| { service.track(); load(false); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "apis.plugin" { load(true); }
        t
    });
    const PATHS: [&str; 8] = ["/service/auth/oauth2_client_credentials/any_client", "/service/auth/oauth2_client_credentials/token_ttl", "/service/billing/enabled", "/service/billing/per_call", "/service/billing/warn_above", "/service/limits/latency", "/service/limits/rate", "/service/limits/fail_share"];
    let dirty = Memo::new(move |_| { let (a, b) = (saved.get(), draft.get()); PATHS.iter().filter(|p| a.pointer(p) != b.pointer(p)).count() });
    let set = move |path: &'static str, v: Value| draft.update(|d| {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut cur = d;
        for (i, k) in parts.iter().enumerate() { if i + 1 == parts.len() { cur[*k] = v.clone(); } else { if !cur[*k].is_object() { cur[*k] = json!({}); } cur = &mut cur[*k]; } }
    });
    let val = move |path: &'static str| draft.get().pointer(path).cloned().unwrap_or(Value::Null);
    let save = move |force: bool| {
        let (sv, d, o) = (service.get_untracked(), draft.get_untracked(), saved.get_untracked());
        let mut b = json!({});
        let ch = |p: &str| o.pointer(p) != d.pointer(p);
        if ch(PATHS[0]) { b["any_client"] = d.pointer(PATHS[0]).cloned().unwrap_or(json!(true)); }
        if ch(PATHS[1]) { b["token_ttl"] = d.pointer(PATHS[1]).cloned().unwrap_or(json!("60m")); }
        if ch(PATHS[2]) { b["billing"] = d.pointer(PATHS[2]).cloned().unwrap_or(json!(false)); }
        if ch(PATHS[3]) { b["per_call"] = d.pointer(PATHS[3]).cloned().unwrap_or(json!(0)); }
        if ch(PATHS[4]) { b["warn_above"] = d.pointer(PATHS[4]).cloned().unwrap_or(json!("")); }
        if ch(PATHS[5]) { b["latency"] = json!(d.pointer(PATHS[5]).and_then(|v| v.as_str()).filter(|x| !x.is_empty()).unwrap_or("none")); }
        if ch(PATHS[6]) { b["rate"] = json!(d.pointer(PATHS[6]).and_then(|v| v.as_str()).filter(|x| !x.is_empty()).unwrap_or("none")); }
        if ch(PATHS[7]) { b["fail_share"] = d.pointer(PATHS[7]).cloned().unwrap_or(json!(0)); }
        if !force { b["version"] = o["version"].clone(); }
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/service", enc(&sv)), b).await {
                Ok(_) => { err.set(String::new()); elsewhere.set(false); saved.set(Value::Null); draft.set(Value::Null); ui::toast("Saved"); load(false); }
                Err(e) if e.contains("changed elsewhere") => elsewhere.set(true),
                Err(e) => err.set(e),
            }
        });
    };
    let expire = move |_| { let sv = service.get_untracked(); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/plugins/{}/tokens/expire", enc(&sv)), json!({})).await { Ok(v) => ui::toast(format!("{} token(s) expired: the app's next call gets 401", api::n(&v, "expired"))), Err(e) => ui::toast_err(e) }
    }); };
    let sel = move |path: &'static str, opts: Vec<(&'static str, &'static str)>| view! {
        <select class="select" class:changed=move || saved.get().pointer(path) != draft.get().pointer(path) on:change=move |e| { let v = event_target_value(&e); set(path, if path.ends_with("fail_share") { json!(v.parse::<f64>().unwrap_or(0.0)) } else { json!(v) }); }>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || { let cur = val(path); if path.ends_with("fail_share") { (cur.as_f64().unwrap_or(0.0) - v.parse::<f64>().unwrap_or(-1.0)).abs() < 1e-9 } else { cur.as_str().unwrap_or("") == v } }>{l}</option> }).collect_view()}
        </select>
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="auth" right=move || view! {
                {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> }) }}
                <button class="btn primary" disabled=move || dirty.get() == 0 on:click=move |_| save(false)>"Save"</button>
            }/> }}
            {move || elsewhere.get().then(|| view! { <div class="strip">{format!("{} changed elsewhere while you edited.", s(&saved.get(), "file"))}
                <button class="link" on:click=move |_| save(true)>"Keep mine"</button><button class="link" on:click=move |_| { elsewhere.set(false); saved.set(Value::Null); draft.set(Value::Null); load(false); }>"Load the file"</button></div> })}
            <div class="body">
                <div class="pane pad form"><div style="max-width:720px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let u = d["urls"].clone();
                        let sv = d["service"].clone();
                        let oauth = !sv["auth"]["oauth2_client_credentials"].is_null();
                        let (l1, l2, l3) = (s(&u, "local"), s(&u, "host"), s(&u, "local"));
                        view! {
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Base URL and host"</div>
                                <div class="row" style="margin-bottom:6px"><span style="width:140px">"Base URL"</span><span class="mono grow">{l1}</span><button class="link" on:click=move |_| ui::copy(&l3, "Base URL")>"Copy"</button></div>
                                <div class="row" style="margin-bottom:6px"><span style="width:140px"></span><span class="mono small cap grow">{l2}</span></div>
                                {(!s(&sv, "real_host").is_empty()).then(|| view! { <div class="row"><span style="width:140px">"Real host"</span><span class="mono grow">{s(&u, "https")}</span></div>
                                    <div class="small cap" style="margin:4px 0 0 148px">"For apps with the host fixed in code: Stuntman also answers on the real host name with its own certificate. Trust the Stuntman Test CA and point that host's DNS at Stuntman (Environment settings › Copy hosts entries)."</div> })}
                                <div class="row" style="margin:8px 0 0 148px;gap:18px"><a class="link" href=api::url("/api/keyvault/test-ca.pem") download="stuntman-test-ca.pem">"Download the test CA"</a>
                                    <span class="small cap">{format!("HTTPS: {}", s(&u, "https"))}</span></div>
                            </div>
                            {oauth.then(|| view! {
                                <div class="block">
                                    <div class="caps" style="margin-bottom:12px">"OAuth2 client credentials"</div>
                                    <div class="row" style="margin-bottom:6px"><span style="width:140px">"Client id"</span><span class="link">{format!("from Secrets › {}", s(&sv["auth"]["oauth2_client_credentials"], "client_id").trim_start_matches("secret://"))}</span></div>
                                    <div class="row" style="margin-bottom:10px"><span style="width:140px">"Client secret"</span><span class="link">{format!("from Secrets › {}", s(&sv["auth"]["oauth2_client_credentials"], "client_secret").trim_start_matches("secret://"))}</span></div>
                                    <div class="row" style="margin-bottom:10px"><Toggle on=Signal::derive(move || val("/service/auth/oauth2_client_credentials/any_client") != false) set=move |v: bool| set("/service/auth/oauth2_client_credentials/any_client", json!(v))/>
                                        <span style="width:110px">"Accept any client"</span><span class="small cap">"off: wrong credentials get 401 invalid_client"</span></div>
                                    <div class="row"><span style="width:140px">"Token lifetime"</span>{sel("/service/auth/oauth2_client_credentials/token_ttl", vec![("60m", "60 min"), ("10m", "10 min"), ("1m", "1 min (test refresh)")])}
                                        <span class="small cap">"an expired token gets 401; the app must ask again"</span>
                                        <button class="link" style="margin-left:auto" on:click=expire>"Expire tokens now"</button></div>
                                </div>
                            })}
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Client certificate (mTLS)"</div>
                                <div class="row" style="margin-bottom:8px"><Toggle on=Signal::derive(move || draft.get()["mtls"] == true) set=move |v: bool| { let sv = service.get_untracked(); leptos::task::spawn_local(async move {
                                        match api::send("PUT", &format!("/plugins/{}/service", enc(&sv)), json!({"client_certificate": v})).await { Ok(_) => { ui::toast(if v { "Calls without a certificate now fail the TLS handshake" } else { "No client certificate required" }); saved.set(Value::Null); draft.set(Value::Null); load(false); } Err(e) => ui::toast_err(e) } }); }/>
                                    <span>"Require a client certificate"</span><span class="small cap">"on HTTPS: none fails the handshake; one not signed by the Stuntman Test CA is “certificate refused”"</span></div>
                                <button class="link" style="margin-left:42px" on:click=move |_| { leptos::task::spawn_local(async move {
                                    match api::send("POST", "/apis/client-certificates", json!({"name": "acme-api"})).await { Ok(v) => ui::copy(&format!("{}{}", s(&v, "certificate"), s(&v, "key")), "Client certificate and key for acme-api"), Err(e) => ui::toast_err(e) } }); }>"Issue a client certificate for the app"</button>
                            </div>
                            <IcpSignature service=service.get() view=d.clone()/>
                            {(!d["session"].is_null()).then(|| { let ss = d["session"].clone(); view! {
                                <div class="block">
                                    <div class="caps" style="margin-bottom:12px">"Session login"</div>
                                    <div class="row" style="margin-bottom:6px"><span style="width:140px">"Cookie"</span><span class="mono">{s(&ss, "cookie")}</span>
                                        <span class="small cap" style="margin-left:12px">{format!("lasts {}  ·  {}", s(&ss, "ttl"), if ss["any_user"] == true { "any user logs in" } else { "only the users in Secrets" })}</span></div>
                                    <div class="row"><span style="width:140px"></span><button class="link" on:click=expire>"Expire sessions now"</button><span class="small cap" style="margin-left:10px">"the portal answers “Session expired”; the app must log in again"</span></div>
                                </div> } })}
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Billing"</div>
                                <div class="row" style="margin-bottom:10px"><Toggle on=Signal::derive(move || val("/service/billing/enabled") == true) set=move |v: bool| set("/service/billing/enabled", json!(v))/><span>"Count billable calls"</span></div>
                                <div class="row" style="margin-bottom:10px"><span style="width:140px">"Price per call"</span>
                                    <input class="input mono" style="width:110px" prop:value=val("/service/billing/per_call").as_f64().unwrap_or(0.0).to_string() on:change=move |e| set("/service/billing/per_call", json!(event_target_value(&e).replace(',', ".").parse::<f64>().unwrap_or(0.0)))/>
                                    <span class="small cap">"USD, only for the counter; nothing is charged"</span></div>
                                <div class="row"><span style="width:140px">"Warn above"</span>{sel("/service/billing/warn_above", vec![("", "No warning"), ("50/day", "50 calls / day"), ("100/day", "100 calls / day"), ("500/day", "500 calls / day")])}
                                    <span class="small cap">"a strip on Calls; useful to catch retry loops"</span></div>
                            </div>
                            <div class="block">
                                <div class="caps" style="margin-bottom:12px">"Limits and failures"</div>
                                <div class="row" style="margin-bottom:10px"><span style="width:140px">"Latency"</span>{sel("/service/limits/latency", vec![("", "None"), ("50ms..150ms", "50–150 ms"), ("300ms..600ms", "300–600 ms"), ("2s..4s", "2–4 s"), ("31s", "31 s (past a 30 s timeout)")])}</div>
                                <div class="row" style="margin-bottom:10px"><span style="width:140px">"Rate limit"</span>{sel("/service/limits/rate", vec![("", "None"), ("1/s", "1 call/s"), ("10/s", "10 calls/s"), ("100/m", "100 calls/min")])}<span class="small cap">"over it: 429"</span></div>
                                <div class="row"><span style="width:140px">"Fail a share"</span>{sel("/service/limits/fail_share", vec![("0", "0 %"), ("0.1", "10 %"), ("0.5", "50 %"), ("1", "100 %")])}
                                    <span class="small cap">"with 503; at 50 % the app's circuit breaker should open"</span></div>
                            </div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                            <div class="small cap">{format!("Fields come from {} (service block). The routes and answers are in the same file.", s(&d, "file"))}</div>
                        }.into_any()
                    }}
                </div></div>
                <div class="side yaml" style="width:430px">
                    <div class="row"><span class="caps">{move || s(&saved.get(), "file")}</span><span class="small ok">"live"</span></div>
                    <pre class="yaml envyaml" style="margin-top:10px">{move || {
                        let text = s(&saved.get(), "yaml");
                        let changed: Vec<&str> = PATHS.iter().filter(|p| saved.get().pointer(p) != draft.get().pointer(p)).map(|p| p.rsplit('/').next().unwrap_or("")).collect();
                        let mut in_service = false;
                        text.lines().map(|l| {
                            if !l.starts_with(' ') && !l.is_empty() { in_service = l.starts_with("service:"); }
                            let key = l.trim_start().split(':').next().unwrap_or("").to_string();
                            let mark = in_service && (changed.contains(&key.as_str()) || (key == "enabled" && changed.contains(&"enabled")));
                            view! { <div class="yl" class:mark=mark class:shade=in_service>{l.to_string()}</div> }
                        }).collect_view()
                    }}</pre>
                    <div class="small cap">"The service block is shaded; changed lines are marked."</div>
                </div>
            </div>
        </div>
    }
}

/// A18 · an ICP-Brasil style certificate on mTLS, the XML signatures of a SOAP service, and the
/// test certificates the app can use.
#[component]
fn IcpSignature(service: String, view: Value) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let certs = RwSignal::new(Vec::<Value>::new());
    let (holder, cnpj) = (RwSignal::new("Acme Transportes Ltda".to_string()), RwSignal::new("11.222.333/0001-81".to_string()));
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/apis/test-certificates").await { certs.set(v["certificates"].as_array().cloned().unwrap_or_default()) } });
    });
    let sv = service.clone();
    let set = move |body: Value| { let sv = sv.clone(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/plugins/{}/service", enc(&sv)), body).await { Ok(_) => ui::toast("Saved"), Err(e) => ui::toast_err(e) } }) };
    let make = move |expired: bool| {
        let body = json!({"holder": holder.get_untracked(), "cnpj": cnpj.get_untracked(), "expired": expired});
        leptos::task::spawn_local(async move {
            match api::send("POST", "/apis/test-certificates", body).await {
                Ok(v) => {
                    // The .pfx downloads; its password is shown once.
                    let href = format!("data:application/x-pkcs12;base64,{}", s(&v, "pfx"));
                    if let Some(d) = web_sys::window().and_then(|w| w.document()) {
                        if let Ok(a) = d.create_element("a") { let _ = a.set_attribute("href", &href); let _ = a.set_attribute("download", &format!("{}.pfx", s(&v, "holder").replace(' ', "-"))); if let Some(h) = a.dyn_ref::<web_sys::HtmlElement>() { h.click() } }
                    }
                    ui::toast(format!(".pfx password (shown once): {}", s(&v, "password")));
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let soap = !view["soap"].is_null();
    let sg = view["signature"].clone();
    let icp = view["icp"] == true;
    let (s1, s2, s3) = (set.clone(), set.clone(), set.clone());
    view! {
        <div class="block">
            <div class="caps" style="margin-bottom:10px">"ICP-Brasil certificate"</div>
            <label class="check"><input type="checkbox" prop:checked=icp on:change=move |e| s1(json!({"client_certificate_icp": event_target_checked(&e)}))/>
                "The app's certificate must carry a CNPJ (OID 2.16.76.1.3.3), as SEFAZ asks. Missing or expired: the TLS handshake fails."</label>
            {soap.then(|| view! {
                <div class="caps" style="margin:16px 0 8px">"XML signature (XMLDSig)"</div>
                <label class="check"><input type="checkbox" prop:checked=sg["check"] == true on:change=move |e| s2(json!({"xml_signature_check": event_target_checked(&e)}))/>"Check the signature of each request (enveloped, C14N, RSA-SHA1 or RSA-SHA256)"</label>
                <div class="row" style="gap:8px;margin-top:6px"><span class="small cap">"Sign the answers' element"</span>
                    <input class="input mono" style="width:160px" placeholder="infProt" prop:value=s(&sg, "sign") on:change=move |e| s3(json!({"xml_signature_sign": event_target_value(&e)}))/>
                    <span class="small cap">"a bad signature answers"</span><span class="mono small">{if s(&sg, "invalid").is_empty() { "a fault".to_string() } else { s(&sg, "invalid") }}</span></div>
            })}
            <div class="caps" style="margin:16px 0 8px">"Test certificates for the app"</div>
            <table class="tbl"><tr><th>"Holder"</th><th>"CNPJ / CPF"</th><th>"Type"</th><th>"State"</th></tr>
                {move || certs.get().into_iter().map(|c| view! { <tr><td>{s(&c, "holder")}</td><td class="mono">{if s(&c, "cnpj").is_empty() { s(&c, "cpf") } else { s(&c, "cnpj") }}</td><td>{s(&c, "type")}</td>
                    <td class={if s(&c, "state") == "valid" { "ok" } else { "danger" }}>{s(&c, "state")}</td></tr> }).collect_view()}</table>
            <div class="row" style="gap:8px;margin-top:8px"><input class="input" style="width:220px" prop:value=move || holder.get() on:input=move |e| holder.set(event_target_value(&e))/>
                <input class="input mono" style="width:170px" prop:value=move || cnpj.get() on:input=move |e| cnpj.set(event_target_value(&e))/>
                <button class="btn small" on:click=move |_| make(false)>"New certificate (.pfx)"</button><button class="btn small" on:click=move |_| make(true)>"An expired one"</button></div>
            <div class="small cap" style="margin-top:6px">"Synthetic holders only. Valid only where the Stuntman Test CA is trusted."</div>
        </div>
    }
}
