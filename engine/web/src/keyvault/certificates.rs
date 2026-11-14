//! K3 · Certificates: subject, issuer, validity, how the app gets it (certificate, secret .pfx, key),
//! downloads, Renew now, who reads it, and the provocations a start-up must survive.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use super::{ymd, VaultCol, events_view, href, readers_view, sel, short, Head, Provoke};
use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

#[component]
pub fn Certificates() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let (vault, item) = sel();
    let d = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let (v, i) = (vault.get(), item.get());
        if v.is_empty() { return; }
        // A slower answer for an earlier selection must not overwrite this one.
        let (v2, i2) = (v.clone(), i.clone());
        leptos::task::spawn_local(async move {
            let name = if i.is_empty() { api::get(&format!("/keyvault/certificates?vault={}", enc(&v))).await.ok().and_then(|x| x["certificates"].as_array().and_then(|a| a.first().map(|k| s(k, "name")))).unwrap_or_default() } else { i };
            if name.is_empty() { d.set(Value::Null); return; }
            if let Ok(x) = api::get(&format!("/keyvault/certificates/{}/detail?vault={}", enc(&name), enc(&v))).await { if vault.get_untracked() == v2 && item.get_untracked() == i2 { d.set(x); } }
        });
    });
    let name = Memo::new(move |_| s(&d.get(), "name"));
    let renew = move |_: ()| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/keyvault/certificates/{}/renew", enc(&n)), json!({"vault": v})).await {
                Ok(x) => ui::toast(format!("{n}: version {} signed by the Test CA", short(&s(&x, "version")))),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    // X4 · New certificate: issue from the Test CA (subject, DNS names, validity) or import a test .pfx / PEM.
    let dlg = RwSignal::new(false);
    let mode = RwSignal::new("issue".to_string());
    let (f_name, f_subject, f_days, f_san) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new("365".to_string()), RwSignal::new(String::new()));
    let sans = RwSignal::new(Vec::<String>::new());
    let file = RwSignal::new(None::<(String, String)>); // (file name, base64 .pfx or PEM text)
    let pwd = RwSignal::new(String::new());
    let names = RwSignal::new(Vec::<String>::new());
    let open_new = move || {
        mode.set("issue".into()); f_name.set(String::new()); f_subject.set(String::new()); f_days.set("365".into()); f_san.set(String::new()); sans.set(vec![]); file.set(None); pwd.set(String::new());
        let v = vault.get_untracked();
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/keyvault/certificates?vault={}", enc(&v))).await { names.set(x["certificates"].as_array().map(|a| a.iter().map(|c| s(c, "name")).collect()).unwrap_or_default()); } });
        dlg.set(true);
    };
    super::drop_new_on_close(dlg);
    let qm_new = leptos_router::hooks::use_query_map();
    Effect::new(move |_| if let Some(n) = qm_new.get().get("new").filter(|n| !n.is_empty()) { open_new(); f_name.set(n); });
    let add_san = move || { let x = f_san.get_untracked().trim().trim_end_matches(',').to_string(); if !x.is_empty() { sans.update(|v| if !v.contains(&x) { v.push(x) }); } f_san.set(String::new()); };
    let pick = move |e: leptos::ev::Event| {
        use wasm_bindgen::JsCast;
        let Some(input) = e.target().and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok()) else { return };
        let Some(f) = input.files().and_then(|l| l.get(0)) else { return };
        let fname = f.name();
        leptos::task::spawn_local(async move {
            match crate::storage::read_file(f).await {
                Ok(bytes) => { let txt = String::from_utf8(bytes.clone()).ok().filter(|t| t.contains("-----BEGIN"));
                    file.set(Some((fname, txt.unwrap_or_else(|| crate::storage::base64(&bytes))))); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let create = move |_| {
        let (n, v) = (f_name.get_untracked().trim().to_string(), vault.get_untracked());
        if n.is_empty() { ui::toast_err("Give the certificate a name"); return; }
        add_san();
        let nav = use_navigate();
        let (path, body) = if mode.get_untracked() == "import" {
            let Some((_, data)) = file.get_untracked() else { ui::toast_err("Pick a .pfx or PEM file"); return };
            (format!("/keyvault/certificates/{}/import", enc(&n)), json!({"vault": v, "pfx_base64": data, "password": pwd.get_untracked()}))
        } else {
            let subj = f_subject.get_untracked();
            let mut b = json!({"vault": v, "days": f_days.get_untracked().parse::<u64>().unwrap_or(365), "san": sans.get_untracked()});
            if !subj.trim().is_empty() { b["subject"] = json!(subj.trim()); }
            (format!("/keyvault/certificates/{}", enc(&n)), b)
        };
        leptos::task::spawn_local(async move {
            match api::send("POST", &path, body).await {
                Ok(_) => { dlg.set(false); ui::toast(format!("Certificate {n} is in {v}")); nav(&href("certificates", &v, &n), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let filter = RwSignal::new(String::new());
    view! {
        <div class="page">
            <Head active="certificates" search=filter placeholder="Name, identity or tag…" right=move || view! { <button class="btn primary" on:click=move |_| open_new()>"New certificate"</button> }/>
            <div class="body">
                <VaultCol tab="certificates" filter=filter/>
                {move || {
                    if vault.get().is_empty() { return super::no_vault().into_any(); }
                    let x = d.get();
                    if x.is_null() {
                        return view! { <div class="pane pad"><div class="callout"><div class="t">"No certificates in this vault"</div>
                            <div class="t2">"New certificate issues one from the Stuntman Test CA, with an exportable RSA key: the app reads it as a certificate, as a .pfx secret or as a key."</div></div></div> }.into_any();
                    }
                    let n = s(&x, "name");
                    let left = api::n(&x, "daysLeft");
                    let soon = left < 30;
                    let vv = vault.get_untracked();
                    let dl = |as_: &str| api::url(&format!("/keyvault/certificates/{}/download?vault={}&as={as_}", enc(&n), enc(&vv)));
                    let near = x["events"].as_array().and_then(|a| a.iter().find(|e| s(e, "event") == "CertificateNearExpiry").cloned());
                    let issuer = s(&x, "issuer");
                    let trusted = issuer.contains("Test CA") && !issuer.contains("Unknown");
                    view! {
                        <div class="pane pad">
                            <h2 class="mono" style="margin:0">{n.clone()}</h2>
                            <div class="small" class:attn={soon && left >= 0} class:danger={left < 0} class:cap={!soon}>
                                {if left < 0 { format!("Expired {} days ago  ·  {}", -left, ymd(api::n(&x, "notAfter"))) } else { format!("Expires in {left} days  ·  {}", ymd(api::n(&x, "notAfter"))) }}</div>
                            <div style="margin-top:16px">{super::state_notes("certificates", &x, &vv, Callback::new(|_| ()), Callback::new(move |_| renew(())), Callback::new(move |_| { let (n, v) = (name.get_untracked(), vault.get_untracked()); leptos::task::spawn_local(async move { match api::send("POST", &format!("/keyvault/certificates/{}/provoke", enc(&n)), json!({"answer": "normal", "vault": v})).await { Ok(_) => ui::toast("Back to normal"), Err(e) => ui::toast_err(e) } }); }))}</div>
                            <table class="kv" style="max-width:640px;margin-top:16px">
                                <tr><td>"Subject"</td><td class="mono">{s(&x, "subject")}</td></tr>
                                <tr><td>"Issuer"</td><td class="mono">{format!("{issuer}{}", if trusted { " (not trusted outside Stuntman)" } else { " (not trusted)" })}</td></tr>
                                <tr><td>"Thumbprint"</td><td class="mono">{s(&x, "thumbprint")}</td></tr>
                                <tr><td>"Valid"</td><td class="mono">{format!("{} → {}", ymd(api::n(&x, "notBefore")), ymd(api::n(&x, "notAfter")))}</td></tr>
                                <tr><td>"Key"</td><td class="mono">{s(&x, "key")}</td></tr>
                                <tr><td>"Renewal"</td><td class="mono">{s(&x, "renewal")}</td></tr>
                            </table>
                            <div class="caps" style="margin-top:24px">"Get it as"</div>
                            <table class="kv" style="max-width:640px">
                                <tr><td>"Certificate"</td><td class="small">{format!("GET /certificates/{n}  ·  public part (.cer)")}</td></tr>
                                <tr><td>"Secret"</td><td class="small">{format!("GET /secrets/{n}  ·  .pfx in base64, with the private key")}</td></tr>
                                <tr><td>"Key"</td><td class="small">{format!("GET /keys/{n}  ·  sign without exporting")}</td></tr>
                            </table>
                            <div class="row" style="margin-top:14px;gap:24px"><a class="link" href=dl("cer")>"Download .cer"</a><a class="link" href=dl("pfx")>"Download .pfx"</a></div>
                            <div class="row" style="margin-top:18px;gap:16px"><button class="link" on:click=move |_| renew(())>"Renew now"</button><span class="small cap">"new version signed by the Stuntman Test CA; CertificateNewVersionCreated"</span></div>
                            {near.map(|e| view! { <div class="callout attn-fill" style="margin-top:18px">{format!("CertificateNearExpiry was sent to subscribers {}. The app should warn or renew; in {left} days signatures made with it will not validate.", api::ago(api::n(&e, "at")))}</div> })}
                        </div>
                        <div class="side">
                            <div class="caps">"Who reads it  ·  last hour"</div>
                            {readers_view(x["readers"].as_array().cloned().unwrap_or_default(), "reads")}
                            <div style="margin-top:12px"><a class="link" href=format!("/secrets/log?vault={}&item={}", enc(&vv), enc(&n))>"Access log for this certificate ›"</a></div>
                            <div class="caps" style="margin-top:28px">"Provoke"</div>
                            <Provoke kind="certificates" name=Signal::derive(move || name.get()) vault=Signal::derive(move || vault.get()) current=Signal::derive(move || s(&d.get()["provoked"], "answer"))
                                options=vec![("normal", "Normal"), ("missing", "Missing (404)"), ("expired", "Expired now"), ("not_yet_valid", "Not yet valid"), ("wrong_password", "Wrong .pfx password"), ("untrusted", "Issuer not trusted")]/>
                            <div class="small cap" style="margin-top:8px">"Missing (404 at start-up), expired now, not yet valid, wrong password on the .pfx, or the issuer not trusted."</div>
                            <div class="small cap" style="margin-top:8px">"A missing certificate at start-up is the classic: the app must fail with a clear message, not crash in a loop."</div>
                            <div class="caps" style="margin-top:28px">"Events"</div>
                            {events_view(x["events"].as_array().cloned().unwrap_or_default(), vec![])}
                        </div>
                    }.into_any()
                }}
            </div>
            <Dialog open=dlg title="New certificate">
                <div class="fd">
                    <div class="small t2">"From the Stuntman Test CA, or your own test .pfx or PEM."</div>
                    <div style="margin-top:14px"><ui::Seg options=vec![("issue", "Issue from the Test CA"), ("import", "Import a .pfx or PEM")] value=Signal::derive(move || mode.get()) set=Callback::new(move |v| mode.set(v))/></div>
                    <div class="caps fd-l">"Name"</div>
                    <input class="input mono" style="width:100%" placeholder="acme-signing-cert" prop:value=move || f_name.get() on:input=move |e| f_name.set(event_target_value(&e))/>
                    {move || if mode.get() == "issue" { view! {
                        <div class="caps fd-l">"Subject"</div>
                        <input class="input mono" style="width:100%" placeholder=move || format!("CN={}", if f_name.get().trim().is_empty() { "acme-signing-cert".to_string() } else { f_name.get().trim().to_string() }) prop:value=move || f_subject.get() on:input=move |e| f_subject.set(event_target_value(&e))/>
                        <div class="caps fd-l">"DNS names (SAN)"</div>
                        <div class="fd-sans">
                            {move || sans.get().into_iter().map(|x| { let x2 = x.clone(); view! { <button class="fd-chip on mono" on:click=move |_| sans.update(|v| v.retain(|y| *y != x2))>{format!("{x}  ×")}</button> } }).collect_view()}
                            <input placeholder="add… (Enter)" prop:value=move || f_san.get() on:input=move |e| f_san.set(event_target_value(&e))
                                on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" || e.key() == "," { e.prevent_default(); add_san(); }/>
                        </div>
                        <div class="caps fd-l">"Valid"</div>
                        <div class="row" style="gap:10px"><ui::Seg options=vec![("30", "30 days"), ("365", "1 year"), ("730", "2 years")] value=Signal::derive(move || f_days.get()) set=Callback::new(move |v| f_days.set(v))/>
                            <span class="small cap">{move || { let now = api::now() as i64 / 1000; format!("{} → {}", ymd(now), ymd(now + f_days.get().parse::<i64>().unwrap_or(365) * 86_400)) }}</span></div>
                        <div class="caps fd-l">"Key"</div>
                        <div class="small">"RSA 2048  ·  exportable (the .pfx carries the private key)"</div>
                    }.into_any() } else { view! {
                        <div class="caps fd-l">"File"</div>
                        <input type="file" accept=".pfx,.p12,.pem,.crt,.key" on:change=pick/>
                        <div class="small cap" style="margin-top:6px">{move || file.get().map(|(n, d)| format!("{n}  ·  {}", if d.contains("-----BEGIN") { "PEM (key and certificate)" } else { ".pfx" })).unwrap_or("A .pfx with its password, or a PEM with the private key and the certificate.".to_string())}</div>
                        <div class="caps fd-l">".pfx password"</div>
                        <input class="input" type="password" style="width:100%" placeholder="empty if none" prop:value=move || pwd.get() on:input=move |e| pwd.set(event_target_value(&e))/>
                    }.into_any() }}
                    <div class="callout attn-fill" style="margin:16px 0 0;max-width:none">
                        <div class="small">{move || if mode.get() == "issue" { "Not trusted outside Stuntman: trust test-ca.pem where the app checks the chain." } else { "Test certificates only: never a production .pfx." }}</div>
                        <div class="mono small ell">{move || { let n = f_name.get(); format!("GET /secrets/{}  →  .pfx in base64, with the private key", if n.trim().is_empty() { "<name>".to_string() } else { n.trim().to_string() }) }}</div>
                    </div>
                    <div class="fd-foot">
                        <span class="small attn grow">{move || { let n = f_name.get().trim().to_string(); if names.get().contains(&n) { format!("{n} exists: this adds a new version.") } else { String::new() } }}</span>
                        <button class="btn" on:click=move |_| dlg.set(false)>"Cancel"</button>
                        <button class="btn primary" on:click=create>{move || if mode.get() == "issue" { "Issue certificate" } else { "Import certificate" }}</button>
                    </div>
                </div>
            </Dialog>
        </div>
    }
}
