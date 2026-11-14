//! K1 · Secrets: the open secret's value (hidden until Show, which is logged), versions with their
//! reads, Rotate now, the files that use it, who reads it and whether it looks cached, next reads.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use serde_json::{json, Value};

use super::{VaultCol, day_s, events_view, href, readers_view, sel, short, Head, Provoke};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Dialog},
};

#[component]
pub fn Secrets() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let (vault, item) = sel();
    let d = RwSignal::new(Value::Null);
    let shown = RwSignal::new(None::<String>);
    let list = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        live.tick.track();
        let (v, i) = (vault.get(), item.get());
        if v.is_empty() { return; }
        // A slower answer for an earlier selection must not overwrite this one.
        let (v2, i2) = (v.clone(), i.clone());
        leptos::task::spawn_local(async move {
            let l = api::get(&format!("/keyvault/secrets?vault={}", enc(&v))).await.ok().and_then(|x| x["secrets"].as_array().cloned()).unwrap_or_default();
            let name = if i.is_empty() { l.first().map(|x| s(x, "name")).unwrap_or_default() } else { i };
            list.set(l);
            if name.is_empty() { d.set(Value::Null); return; }
            if let Ok(x) = api::get(&format!("/keyvault/secrets/{}/detail?vault={}", enc(&name), enc(&v))).await { if vault.get_untracked() == v2 && item.get_untracked() == i2 { d.set(x); } }
        });
    });
    Effect::new(move |_| { item.track(); vault.track(); shown.set(None); });
    let name = Memo::new(move |_| s(&d.get(), "name"));
    // X2 · Revealing a secret: asked first, then recorded in the access log as you.
    let ask_show = RwSignal::new(false);
    let show = move |_| { if shown.get_untracked().is_some() { shown.set(None); } else { ask_show.set(true); } };
    let reveal = move |_| {
        ask_show.set(false);
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::get(&format!("/keyvault/secrets/{}?vault={}&by=you", enc(&n), enc(&v))).await { Ok(x) => shown.set(Some(s(&x, "value"))), Err(e) => ui::toast_err(e) }
        });
    };
    // X4 · New secret and Rotate: synthetic value (Generate), content type, expiry, tags; who uses the name;
    // rotating can disable the version it replaces.
    let dlg = RwSignal::new(false);
    let rot = RwSignal::new(false);
    let (f_name, f_value, f_ct, f_exp, f_tags) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new("text/plain".to_string()), RwSignal::new("never".to_string()), RwSignal::new(String::new()));
    let disable_old = RwSignal::new(false);
    let used = RwSignal::new(Vec::<Value>::new());
    Effect::new(move |_| {
        let n = f_name.get().trim().to_string();
        if !dlg.get() || n.is_empty() { used.set(vec![]); return; }
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/keyvault/used-by/{}", enc(&n))).await { used.set(x["usedBy"].as_array().cloned().unwrap_or_default()); } });
    });
    let open_new = move |r: bool| {
        rot.set(r); f_name.set(if r { name.get_untracked() } else { String::new() }); f_value.set(String::new());
        f_ct.set(if r { let c = s(&d.get_untracked()["versions"][0], "contentType"); if c.is_empty() { "text/plain".into() } else { c } } else { "text/plain".into() });
        f_exp.set("never".into()); disable_old.set(false);
        f_tags.set(if r { d.get_untracked()["versions"][0]["tags"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ")).unwrap_or_default() } else { String::new() });
        dlg.set(true);
    };
    let save = move |_| {
        let (n, val, ct, exp, tags, v) = (f_name.get_untracked().trim().to_string(), f_value.get_untracked(), f_ct.get_untracked(), f_exp.get_untracked(), f_tags.get_untracked(), vault.get_untracked());
        if n.is_empty() || val.is_empty() { ui::toast_err("A name and a value"); return; }
        let old = if rot.get_untracked() && disable_old.get_untracked() { s(&d.get_untracked()["versions"][0], "id") } else { String::new() };
        let nav = use_navigate();
        let tags: serde_json::Map<String, Value> = tags.split(',').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.trim().to_string(), json!(v.trim()))).filter(|(k, _)| !k.is_empty()).collect();
        leptos::task::spawn_local(async move {
            let mut b = json!({"value": val, "vault": v, "by": "you", "content_type": ct, "tags": tags});
            if exp != "never" { b["expires"] = json!(exp); }
            match api::send("PUT", &format!("/keyvault/secrets/{}", enc(&n)), b).await {
                Ok(x) => {
                    if !old.is_empty() { let _ = api::send("PATCH", &format!("/keyvault/secrets/{}", enc(&n)), json!({"enabled": false, "version": old, "vault": v})).await; }
                    dlg.set(false); ui::toast(format!("{n}: version {} is current", short(&s(&x, "version")))); nav(&href("secrets", &v, &n), Default::default());
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    // "Create <name>" from the names asked for but missing (the column links here with ?new=<name>).
    super::drop_new_on_close(dlg);
    let qm_new = leptos_router::hooks::use_query_map();
    Effect::new(move |_| if let Some(n) = qm_new.get().get("new").filter(|n| !n.is_empty()) { open_new(false); f_name.set(n); });
    let set_state = move |enabled: bool| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/keyvault/secrets/{}", enc(&n)), json!({"enabled": enabled, "vault": v})).await {
                Ok(_) => ui::toast(if enabled { "Current version enabled" } else { "Current version disabled: reads get 403 SecretDisabled" }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let del = RwSignal::new(false);
    let do_del = move |_| {
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/keyvault/secrets/{}?vault={}", enc(&n), enc(&v)), Value::Null).await {
                Ok(_) => { ui::toast(format!("{n} deleted: recoverable for the soft-delete days")); nav(&href("secrets", &v, ""), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let filter = RwSignal::new(String::new());
    view! {
        <div class="page">
            <Head active="secrets" search=filter placeholder="Secret name or tag…" right=move || view! { <button class="btn primary" on:click=move |_| open_new(false)>"New secret"</button> }/>
            <div class="body">
                <VaultCol tab="secrets" filter=filter/>
                {move || {
                    if vault.get().is_empty() { return super::no_vault().into_any(); }
                    let x = d.get();
                    if x.is_null() {
                        return view! { <div class="pane pad"><div class="callout"><div class="t">"No secrets in this vault"</div>
                            <div class="t2">"Add one with New secret, or let the app write it (PUT /secrets/<name>). Values are synthetic: never a real token."</div></div></div> }.into_any();
                    }
                    let cur = x["versions"].as_array().and_then(|a| a.first().cloned()).unwrap_or(Value::Null);
                    let enabled = cur["enabled"] != json!(false);
                    let tags = x["versions"][0]["tags"].as_object().map(|o| o.iter().map(|(k, v)| format!("tag {k}={}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("  ·  ")).unwrap_or_default();
                    let ct = s(&cur, "contentType");
                    let used: Vec<Value> = x["usedBy"].as_array().cloned().unwrap_or_default();
                    let n_for_link = s(&x, "name");
                    let vv = vault.get_untracked();
                    let prov = s(&x["provoked"], "answer");
                    view! {
                        <div class="pane pad">
                            <h2 class="mono" style="margin:0">{s(&x, "name")}</h2>
                            <div class="small cap">{format!("content type {}{}", if ct.is_empty() { "—".to_string() } else { ct }, if tags.is_empty() { String::new() } else { format!("  ·  {tags}") })}</div>
                            <div style="margin-top:16px">{super::state_notes("secrets", &x, &vv, Callback::new(move |_| set_state(true)), Callback::new(|_| ()), Callback::new(move |_| { let (n, v) = (name.get_untracked(), vault.get_untracked()); leptos::task::spawn_local(async move { match api::send("POST", &format!("/keyvault/secrets/{}/provoke", enc(&n)), json!({"answer": "normal", "vault": v})).await { Ok(_) => ui::toast("Back to normal"), Err(e) => ui::toast_err(e) } }); }))}</div>
                            <div class="row" style="margin-top:18px;gap:16px;max-width:640px">
                                <span style="width:110px">"Current value"</span>
                                <div class="copybox grow">{move || shown.get().unwrap_or_else(|| "••••••••••••••••••".into())}</div>
                                <button class="link" on:click=show>{move || if shown.get().is_some() { "Hide" } else { "Show" }}</button>
                                {move || shown.get().map(|v| view! { <button class="link" on:click=move |_| ui::copy(&v, "Value")>"Copy"</button> })}
                            </div>
                            <div class="small cap" style="margin-top:8px">"Synthetic values only: never paste a real token into Stuntman. Show is written to the access log."</div>
                            <div class="caps" style="margin-top:28px">"Versions"</div>
                            <div class="ftable" style="margin:8px -32px 0">
                                <div class="fhead caps"><span style="width:130px">"Version"</span><span style="width:110px">"Created"</span><span style="width:130px">"State"</span><span style="width:110px">"Expires"</span><span>"Reads"</span></div>
                                {x["versions"].as_array().cloned().unwrap_or_default().into_iter().map(|v| {
                                    let st = s(&v, "state");
                                    let cls = match st.as_str() { "current" => "ok", "expired" => "danger", _ => "cap" };
                                    view! { <div class="frow"><span class="mono" style="width:130px">{short(&s(&v, "id"))}</span><span style="width:110px">{day_s(api::n(&v, "created"))}</span>
                                        <span class=cls style="width:130px">{st}</span><span style="width:110px">{if v["exp"].is_null() { "never".into() } else { day_s(api::n(&v, "exp")) }}</span><span class="mono">{api::thousands(api::n(&v, "reads"))}</span></div> }
                                }).collect_view()}
                            </div>
                            <div class="row" style="margin-top:14px;gap:16px">
                                <button class="link" on:click=move |_| open_new(true)>"Rotate now"</button>
                                <span class="small cap">"adds a version and sends SecretNewVersionCreated to subscribers"</span>
                            </div>
                            <div class="row" style="margin-top:8px;gap:16px">
                                <button class="link" on:click=move |_| set_state(!enabled)>{if enabled { "Disable current version" } else { "Enable current version" }}</button>
                                <button class="link danger" on:click=move |_| del.set(true)>"Delete secret"</button>
                            </div>
                            <div class="caps" style="margin-top:28px">"Used by"</div>
                            {if used.is_empty() { view! { <div class="small cap" style="margin-top:8px">{format!("No file in this environment points at secret://{}.", n_for_link)}</div> }.into_any() } else {
                                used.into_iter().map(|u| view! { <div style="margin-top:8px"><span class="mono">{s(&u, "file")}</span><span class="cap">"  ›  "</span><span class="mono">{s(&u, "field")}</span></div> }).collect_view().into_any() }}
                            <div class="small cap" style="margin-top:6px">"Stuntman's own stand-ins read secret://<name> from this vault, so the app and the stand-in agree."</div>
                        </div>
                        <div class="side">
                            <div class="caps">"Who reads it  ·  last hour"</div>
                            {readers_view(x["readers"].as_array().cloned().unwrap_or_default(), "reads")}
                            <div style="margin-top:12px"><a class="link" href=format!("/secrets/log?vault={}&item={}", enc(&vv), enc(&n_for_link))>"Access log for this secret ›"</a></div>
                            <div class="caps" style="margin-top:28px">"Next reads"</div>
                            <Provoke kind="secrets" name=Signal::derive(move || name.get()) vault=Signal::derive(move || vault.get()) current=Signal::derive(move || s(&d.get()["provoked"], "answer"))
                                options=vec![("normal", "Normal"), ("403", "403 Forbidden"), ("404", "404 Not found"), ("disabled", "Disabled"), ("429", "429 Throttled"), ("slow", "Slow (3 s)")]/>
                            {(!prov.is_empty()).then(|| view! { <div class="small provoked" style="margin-top:6px">{format!("{}  ·  {}", prov, if x["provoked"]["until"].is_null() { "until changed".to_string() } else { format!("for {}", api::span(api::n(&x["provoked"], "until") - api::now() as i64)) })}</div> })}
                            <div class="small cap" style="margin-top:8px">"Make the next reads of this secret fail: 403, 404, disabled, 429 or slow. The app should fail clearly at start-up and keep its cached value later."</div>
                            <div class="caps" style="margin-top:28px">"Events"</div>
                            {events_view(x["events"].as_array().cloned().unwrap_or_default(), x["subscribers"].as_array().cloned().unwrap_or_default())}
                        </div>
                    }.into_any()
                }}
            </div>
            <Dialog open=dlg title="">
                <div class="fd">
                    {move || if rot.get() {
                        let x = d.get();
                        let cur = x["versions"][0].clone();
                        let readers = x["readers"].as_array().cloned().unwrap_or_default();
                        let subs = x["subscribers"].as_array().map(|a| a.len()).unwrap_or(0);
                        view! {
                            <h2 style="margin:0 0 4px">{format!("Rotate {}", s(&x, "name"))}</h2>
                            <div class="small t2">"A new version becomes current; readers get it on their next read."</div>
                            <div class="fd-now"><div class="mono small">{format!("{}  ·  current since {}", short(&s(&cur, "id")), day_s(api::n(&cur, "created")))}</div>
                                <div class="small t2">{if readers.is_empty() { "Nobody read it in the last hour.".to_string() } else { format!("read {} time(s) in the last hour by {}", readers.iter().map(|r| api::n(r, "reads")).sum::<i64>(), readers.iter().map(|r| { let c = s(r, "client"); if c.is_empty() { s(r, "identity") } else { c } }).collect::<Vec<_>>().join(" and ")) }}</div></div>
                            <div class="caps fd-l">"New value"</div>
                            <div class="fd-inbox"><input class="input mono" placeholder="synthetic value" prop:value=move || f_value.get() on:input=move |e| f_value.set(event_target_value(&e))/>
                                <button class="link" on:click=move |_| f_value.set(super::synthetic("demo"))>"Generate"</button></div>
                            <div class="caps fd-l">"The current version"</div>
                            <div class="row" style="gap:10px"><ui::Seg options=vec![("keep", "Keep it enabled"), ("disable", "Disable it")] value=Signal::derive(move || if disable_old.get() { "disable".to_string() } else { "keep".to_string() }) set=Callback::new(move |v: String| disable_old.set(v == "disable"))/>
                                <span class="small cap">"disabled: reads by its id get 403"</span></div>
                            <div class="callout ok-fill" style="margin:16px 0 0;max-width:none"><span class="small">{if subs == 0 { "SecretNewVersionCreated: no subscriber yet (Vault settings).".to_string() } else { format!("SecretNewVersionCreated goes to {subs} subscriber(s).") }}</span></div>
                        }.into_any()
                    } else { view! {
                        <h2 style="margin:0 0 4px">"New secret"</h2>
                        <div class="small t2">{move || format!("A synthetic value in {}. Real tokens never go into Stuntman.", vault.get())}</div>
                        <div class="caps fd-l">"Name"</div>
                        <input class="input mono" style="width:100%" placeholder="smtp-password" prop:value=move || f_name.get() on:input=move |e| f_name.set(event_target_value(&e))/>
                        <div class="caps fd-l">"Value"</div>
                        <div class="fd-inbox"><input class="input mono" placeholder="synthetic value" prop:value=move || f_value.get() on:input=move |e| f_value.set(event_target_value(&e))/>
                            <button class="link" on:click=move |_| f_value.set(super::synthetic("demo"))>"Generate"</button></div>
                        <div class="small cap" style="margin-top:6px">"Looks like a real token (sk_live…, AKIA…, a long JWT)? It is refused."</div>
                        <div class="caps fd-l">"Content type"</div>
                        <ui::Seg options=vec![("text/plain", "text/plain"), ("application/json", "application/json"), ("connection string", "connection string")] value=Signal::derive(move || f_ct.get()) set=Callback::new(move |v| f_ct.set(v))/>
                        <div class="caps fd-l">"Expires"</div>
                        <div class="row" style="gap:10px"><ui::Seg options=vec![("never", "Never"), ("30d", "In 30 days"), ("90d", "In 90 days"), ("365d", "In 1 year")] value=Signal::derive(move || f_exp.get()) set=Callback::new(move |v| f_exp.set(v))/>
                            <span class="small cap">"SecretNearExpiry 30 days before"</span></div>
                        <div class="caps fd-l">"Tags"</div>
                        <input class="input mono" style="width:100%" placeholder="owner=messaging   (key=value, comma between)" prop:value=move || f_tags.get() on:input=move |e| f_tags.set(event_target_value(&e))/>
                        <div class="callout ok-fill" style="margin:16px 0 0;max-width:none"><span class="small">{move || { let n = f_name.get(); let n = if n.trim().is_empty() { "<name>".to_string() } else { n.trim().to_string() };
                            let u = used.get();
                            format!("The app reads it with  GET /secrets/{n}{}", if u.is_empty() { String::new() } else { format!("  ·  used by {}", u.iter().map(|x| format!("{} › {}", s(x, "file"), s(x, "field"))).collect::<Vec<_>>().join(", ")) }) }}</span></div>
                    }.into_any() }}
                    <div class="fd-foot">
                        <span class="small grow" class:attn=move || !rot.get() && list.get().iter().any(|x| s(x, "name") == f_name.get().trim()) class:cap=move || rot.get() || !list.get().iter().any(|x| s(x, "name") == f_name.get().trim())>
                            {move || if !rot.get() && list.get().iter().any(|x| s(x, "name") == f_name.get().trim()) { format!("{} exists: this adds a new version.", f_name.get().trim()) } else { String::new() }}</span>
                        <button class="btn" on:click=move |_| dlg.set(false)>"Cancel"</button>
                        <button class="btn primary" on:click=save>{move || if rot.get() { format!("Save version {}", d.get()["versions"].as_array().map(|a| a.len() + 1).unwrap_or(1)) } else { "Create secret".to_string() }}</button>
                    </div>
                </div>
            </Dialog>
            <Dialog open=ask_show title="">
                <h2 style="margin:0 0 10px">{move || format!("Show the value of {}?", name.get())}</h2>
                <div class="t2">"Values are synthetic, but showing one is still an action others can see."</div>
                <div class="small attn" style="margin-top:14px">"Recorded in the access log as you, now."</div>
                <div class="actions"><button class="btn" on:click=move |_| ask_show.set(false)>"Cancel"</button><button class="btn primary" on:click=reveal>"Show"</button></div>
            </Dialog>
            <Confirm open=del question=Signal::derive(move || format!("Delete {}?", name.get())) detail="It stays recoverable for the vault's soft-delete days; creating the same name gets 409 Conflict until then." action="Delete" on_yes=do_del/>
        </div>
    }
}
