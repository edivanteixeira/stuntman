//! K2 · Keys: operations in the last hour (they really run: signatures verify with the public key),
//! versions, the public key as JWK or PEM, Rotate now, who uses it, next operations.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use super::{VaultCol, day_s, href, readers_view, sel, short, Head, Provoke};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Dialog},
};

#[component]
pub fn Keys() -> impl IntoView {
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
            let name = if i.is_empty() { api::get(&format!("/keyvault/keys?vault={}", enc(&v))).await.ok().and_then(|x| x["keys"].as_array().and_then(|a| a.first().map(|k| s(k, "name")))).unwrap_or_default() } else { i };
            if name.is_empty() { d.set(Value::Null); return; }
            if let Ok(x) = api::get(&format!("/keyvault/keys/{}/detail?vault={}", enc(&name), enc(&v))).await { if vault.get_untracked() == v2 && item.get_untracked() == i2 { d.set(x); } }
        });
    });
    let name = Memo::new(move |_| s(&d.get(), "name"));
    let rotate = move |_| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        let size = api::n(&d.get_untracked(), "size");
        let ops = d.get_untracked()["allowed"].clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/keyvault/keys/{}", enc(&n)), json!({"vault": v, "size": size, "ops": ops})).await {
                Ok(x) => ui::toast(format!("{n}: version {} is current", short(&s(&x, "version")))),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let set_state = move |enabled: bool| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/keyvault/keys/{}", enc(&n)), json!({"enabled": enabled, "vault": v})).await {
                Ok(_) => ui::toast(if enabled { "Key enabled" } else { "Key disabled: operations get 403 KeyDisabled" }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let set_version = move |ver: String, enabled: bool| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/keyvault/keys/{}", enc(&n)), json!({"enabled": enabled, "version": ver, "vault": v})).await {
                Ok(_) => ui::toast(format!("Version {} {}", short(&ver), if enabled { "enabled" } else { "disabled: get and operations on it get 403" })),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let del = RwSignal::new(false);
    let do_del = move |_| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/keyvault/keys/{}?vault={}", enc(&n), enc(&v)), Value::Null).await {
                Ok(_) => { ui::toast(format!("{n} deleted: recoverable for the soft-delete days")); nav(&href("keys", &v, ""), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let pem = move |_| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::get(&format!("/keyvault/keys/{}/public?vault={}&format=pem", enc(&n), enc(&v))).await { Ok(x) => ui::copy(&s(&x, "pem"), "PEM"), Err(e) => ui::toast_err(e) }
        });
    };
    // K2 · New key dialog: name, purpose (sets key_ops), key size, expiry, tags; an existing name rotates.
    let dlg = RwSignal::new(false);
    let f_name = RwSignal::new(String::new());
    let f_purpose = RwSignal::new("sign".to_string());
    let f_ops = RwSignal::new(vec!["sign".to_string(), "verify".to_string()]);
    let f_size = RwSignal::new("2048".to_string());
    let f_exp = RwSignal::new("never".to_string());
    let f_tags = RwSignal::new(String::new());
    let names = RwSignal::new(Vec::<String>::new());
    let all = super::vaults();
    let open_new = move || {
        f_name.set(String::new()); f_purpose.set("sign".into()); f_ops.set(vec!["sign".into(), "verify".into()]); f_size.set("2048".into()); f_exp.set("never".into()); f_tags.set(String::new());
        let v = vault.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/keyvault/keys?vault={}", enc(&v))).await { names.set(x["keys"].as_array().map(|a| a.iter().map(|k| s(k, "name")).collect()).unwrap_or_default()); }
        });
        dlg.set(true);
    };
    super::drop_new_on_close(dlg);
    let qm_new = leptos_router::hooks::use_query_map();
    Effect::new(move |_| if let Some(n) = qm_new.get().get("new").filter(|n| !n.is_empty()) { open_new(); f_name.set(n); });
    let create = move |_| {
        let (n, size, ops, exp, tags, v) = (f_name.get_untracked().trim().to_string(), f_size.get_untracked(), f_ops.get_untracked(), f_exp.get_untracked(), f_tags.get_untracked(), vault.get_untracked());
        if n.is_empty() { ui::toast_err("Give the key a name"); return; }
        if ops.is_empty() { ui::toast_err("Allow at least one operation"); return; }
        let nav = use_navigate();
        let tags: serde_json::Map<String, Value> = tags.split(',').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), json!(v.trim()))).filter(|(k, _)| !k.is_empty()).collect();
        leptos::task::spawn_local(async move {
            let b = json!({"vault": v, "size": size.parse::<u64>().unwrap_or(2048), "ops": ops, "expires": exp, "tags": tags});
            match api::send("POST", &format!("/keyvault/keys/{}", enc(&n)), b).await {
                Ok(x) => { dlg.set(false); ui::toast(format!("{n}: version {} is current", short(&s(&x, "version")))); nav(&href("keys", &v, &n), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let filter = RwSignal::new(String::new());
    view! {
        <div class="page">
            <Head active="keys" search=filter placeholder="Name, identity or tag…" right=move || view! { <button class="btn primary" on:click=move |_| open_new()>"New key"</button> }/>
            <div class="body">
                <VaultCol tab="keys" filter=filter/>
                {move || {
                    if vault.get().is_empty() { return super::no_vault().into_any(); }
                    let x = d.get();
                    if x.is_null() {
                        return view! { <div class="pane pad"><div class="callout"><div class="t">"No keys in this vault"</div>
                            <div class="t2">"New key makes an RSA key that signs, verifies, wraps or encrypts for real, so the app's crypto code is exercised."</div></div></div> }.into_any();
                    }
                    let allowed: Vec<String> = x["allowed"].as_array().map(|a| a.iter().filter_map(|y| y.as_str().map(|z| z.to_lowercase())).collect()).unwrap_or_default();
                    let enabled = x["versions"][0]["enabled"] != json!(false);
                    let jwk = serde_json::to_string_pretty(&x["jwk"]).unwrap_or_default();
                    let j2 = jwk.clone();
                    let n_link = s(&x, "name");
                    let vv = vault.get_untracked();
                    let prov = s(&x["provoked"], "answer");
                    view! {
                        <div class="pane pad">
                            <h2 class="mono" style="margin:0">{s(&x, "name")}</h2>
                            <div class="small cap">{format!("{} {}  ·  allowed: {}  ·  HSM: no (software key){}{}{}", s(&x, "kty"), api::n(&x, "size"), allowed.join(", "), if enabled { "" } else { "  ·  disabled" },
                                x["versions"][0]["exp"].as_i64().map(|e| format!("  ·  expires {}", super::ymd(e))).unwrap_or_default(),
                                x["versions"][0]["tags"].as_object().map(|o| o.iter().map(|(k, v)| format!("  ·  tag {k}={}", v.as_str().unwrap_or(""))).collect::<String>()).unwrap_or_default())}</div>
                            <div style="margin-top:16px">{super::state_notes("keys", &x, &vv, Callback::new(move |_| set_state(true)), Callback::new(|_| ()), Callback::new(move |_| { let (n, v) = (name.get_untracked(), vault.get_untracked()); leptos::task::spawn_local(async move { match api::send("POST", &format!("/keyvault/keys/{}/provoke", enc(&n)), json!({"answer": "normal", "vault": v})).await { Ok(_) => ui::toast("Back to normal"), Err(e) => ui::toast_err(e) } }); }))}</div>
                            <div class="caps" style="margin-top:24px">"Operations  ·  last hour"</div>
                            <table class="kv" style="max-width:520px">
                                {x["operations"].as_array().cloned().unwrap_or_default().into_iter().map(|o| {
                                    let op = s(&o, "op");
                                    let ok = allowed.iter().any(|a| *a == op.to_lowercase());
                                    let p95 = o["p95"].as_i64();
                                    view! { <tr><td>{op}</td>
                                        <td class="mono">{if ok { api::thousands(api::n(&o, "count")) } else { "not allowed".into() }}</td>
                                        <td class="small cap">{p95.filter(|_| ok).map(|p| format!("p95 {p} ms")).unwrap_or_default()}</td></tr> }
                                }).collect_view()}
                            </table>
                            <div class="callout ok-fill" style="margin-top:16px">"Operations really run. Signatures verify with the public key below and wrapped keys unwrap again, so the app's crypto code is exercised."</div>
                            <div class="caps" style="margin-top:24px">"Versions"</div>
                            {x["versions"].as_array().cloned().unwrap_or_default().into_iter().map(|v| {
                                let st = s(&v, "state");
                                let (id, on) = (s(&v, "id"), v["enabled"] != json!(false));
                                view! { <div class="row" style="margin-top:8px;gap:24px"><span class="mono" style="width:120px">{short(&id)}</span><span style="width:90px">{day_s(api::n(&v, "created"))}</span><span style="width:110px" class:ok={st == "current"} class:cap={st != "current"}>{st.clone()}</span>
                                    <button class="link" on:click=move |_| set_version(id.clone(), !on)>{if on { "Disable" } else { "Enable" }}</button></div> }
                            }).collect_view()}
                            <div class="small cap" style="margin-top:8px">"Signatures made with an older version still verify; new ones use the current."</div>
                            <div class="caps" style="margin-top:24px">"Public key"</div>
                            <pre class="yaml" style="max-width:640px;white-space:pre-wrap;word-break:break-all">{jwk}</pre>
                            <div class="row" style="gap:20px"><button class="link" on:click=move |_| ui::copy(&j2, "JWK")>"Copy as JWK"</button><button class="link" on:click=pem>"Copy as PEM"</button></div>
                            <div class="row" style="margin-top:18px;gap:16px"><button class="link" on:click=rotate>"Rotate now"</button><span class="small cap">"new version; KeyNewVersionCreated to subscribers"</span></div>
                            <div class="row" style="margin-top:8px;gap:20px"><button class="link" on:click=move |_| set_state(!enabled)>{if enabled { "Disable key" } else { "Enable key" }}</button>
                                <button class="link danger" on:click=move |_| del.set(true)>"Delete key"</button></div>
                        </div>
                        <div class="side">
                            <div class="caps">"Who uses it  ·  last hour"</div>
                            {readers_view(x["readers"].as_array().cloned().unwrap_or_default(), "ops")}
                            <div style="margin-top:12px"><a class="link" href=format!("/secrets/log?vault={}&item={}", enc(&vv), enc(&n_link))>"Access log for this key ›"</a></div>
                            <div class="caps" style="margin-top:28px">"Next operations"</div>
                            <Provoke kind="keys" name=Signal::derive(move || name.get()) vault=Signal::derive(move || vault.get()) current=Signal::derive(move || s(&d.get()["provoked"], "answer"))
                                options=vec![("normal", "Normal"), ("disabled", "Key disabled"), ("403", "403 Forbidden"), ("429", "429 Throttled"), ("slow", "Slow (2 s)"), ("wrong_signature", "Wrong signature")]/>
                            {(!prov.is_empty()).then(|| view! { <div class="small provoked" style="margin-top:6px">{prov}</div> })}
                            <div class="small cap" style="margin-top:8px">"Make the next operations on this key fail: disabled key, 403, 429, slow 2 s, or a wrong signature on purpose (verify fails)."</div>
                        </div>
                    }.into_any()
                }}
            </div>
            <Confirm open=del question=Signal::derive(move || format!("Delete {}?", name.get())) detail="GET answers 404 KeyNotFound; it stays under Deleted, recoverable for the vault's soft-delete days, and creating the same name gets 409 Conflict until then." action="Delete" on_yes=do_del/>
            <Dialog open=dlg title="New key">
                <div class="fd">
                    <div class="small t2">{move || format!("Made and kept in {}. The private part never leaves Stuntman: the app signs, verifies or wraps through the vault, as with Azure.", vault.get())}</div>
                    <div class="caps fd-l">"Name"</div>
                    <input class="input mono" style="width:100%" placeholder="document-signing" prop:value=move || f_name.get() on:input=move |e| f_name.set(event_target_value(&e))/>
                    <div class="mono small cap ell" style="margin-top:6px">{move || { let n = f_name.get(); format!("{}/keys/{}", s(&super::current(&all.get(), Some(vault.get())), "url"), if n.trim().is_empty() { "<name>".to_string() } else { n.trim().to_string() }) }}</div>
                    <div class="caps fd-l">"What it is for"</div>
                    <div class="fd-cards">
                        {[("sign", "Sign and verify", "PDF signatures, JWTs, webhooks", ["sign", "verify"]), ("wrap", "Wrap and unwrap", "Envelope encryption of data keys", ["wrapKey", "unwrapKey"]), ("encrypt", "Encrypt and decrypt", "Small payloads, up to 190 bytes", ["encrypt", "decrypt"])].into_iter().map(|(id, title, desc, ops)| view! {
                            <button class="fd-card" class:on=move || f_purpose.get() == id on:click=move |_| { f_purpose.set(id.into()); f_ops.set(ops.iter().map(|x| x.to_string()).collect()); }>
                                <span class="fd-radio"></span><b>{title}</b>
                                <span class="small t2">{desc}</span>
                                <span class="mono small">{ops.join("  ·  ")}</span>
                            </button>
                        }).collect_view()}
                    </div>
                    <div class="caps fd-l">"Allowed operations  ·  key_ops"</div>
                    <div class="row" style="gap:8px;flex-wrap:wrap">
                        {["sign", "verify", "wrapKey", "unwrapKey", "encrypt", "decrypt"].into_iter().map(|op| {
                            let on = move || f_ops.get().iter().any(|x| x == op);
                            view! { <button class="fd-chip mono" class:on=on on:click=move |_| f_ops.update(|v| if v.iter().any(|x| x == op) { v.retain(|x| x != op) } else { v.push(op.into()) })>{move || format!("{} {op}", if on() { "✓" } else { "+" })}</button> }
                        }).collect_view()}
                    </div>
                    <div class="small cap" style="margin-top:6px">"Set by the purpose; change any. Calls outside them get 403, as key_ops does in Azure."</div>
                    <div class="caps fd-l">"Key"</div>
                    <div class="row" style="gap:10px"><ui::Seg options=vec![("2048", "RSA 2048"), ("3072", "RSA 3072"), ("4096", "RSA 4096")] value=Signal::derive(move || f_size.get()) set=Callback::new(move |v| f_size.set(v))/>
                        <span class="small cap">"EC P-256 is not in Stuntman yet"</span></div>
                    <div class="caps fd-l">"Expires"</div>
                    <div class="row" style="gap:10px"><ui::Seg options=vec![("never", "Never"), ("365d", "In 1 year"), ("730d", "In 2 years")] value=Signal::derive(move || f_exp.get()) set=Callback::new(move |v| f_exp.set(v))/>
                        <span class="small cap">{move || match f_exp.get().as_str() { "never" => "operations keep working until you disable it".to_string(),
                            d => { let days: i64 = d.trim_end_matches('d').parse().unwrap_or(0); format!("{}  ·  after it, operations get 403 KeyExpired", super::ymd(api::now() as i64 / 1000 + days * 86_400)) } }}</span></div>
                    <div class="caps fd-l">"Tags"</div>
                    <input class="input mono" style="width:100%" placeholder="owner=signing   (key=value, comma between)" prop:value=move || f_tags.get() on:input=move |e| f_tags.set(event_target_value(&e))/>
                    <div class="callout ok-fill" style="margin:16px 0 0;max-width:none">
                        <div class="small">{move || match f_purpose.get().as_str() { "wrap" => "The app wraps a data key with", "encrypt" => "The app encrypts with", _ => "The app signs with" }}</div>
                        <div class="mono small">{move || { let n = f_name.get(); let n = if n.trim().is_empty() { "<name>".to_string() } else { n.trim().to_string() };
                            match f_purpose.get().as_str() { "wrap" => format!("POST /keys/{n}/wrapkey  {{\"alg\": \"RSA-OAEP\", \"value\": <key>}}"), "encrypt" => format!("POST /keys/{n}/encrypt  {{\"alg\": \"RSA-OAEP\", \"value\": <data>}}"), _ => format!("POST /keys/{n}/sign  {{\"alg\": \"RS256\", \"value\": <digest>}}") } }}</div>
                    </div>
                    <div class="fd-foot">
                        <span class="small grow" class:attn=move || names.get().contains(&f_name.get().trim().to_string()) class:cap=move || !names.get().contains(&f_name.get().trim().to_string())>
                            {move || if names.get().contains(&f_name.get().trim().to_string()) { format!("{} exists: Create adds a new version (rotation).", f_name.get().trim()) } else { "Already exists? Create adds a version (rotation).".to_string() }}</span>
                        <button class="btn" on:click=move |_| dlg.set(false)>"Cancel"</button><button class="btn primary" on:click=create>"Create key"</button>
                    </div>
                </div>
            </Dialog>
        </div>
    }
}
