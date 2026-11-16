//! E3 · Services › E-mail: how the app sends (SendGrid API, SMTP), who can send, addresses, the
//! events the app receives, limits. The YAML file sits beside the form; both are the same file.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::{json, Value};

use super::ServicesHead;
use crate::{
    api::{self, s},
    ui::{self, Confirm, Dialog, Pop, Toggle},
};

/// Leaf paths that differ between two JSON values: "events.to".
fn changes(a: &Value, b: &Value, path: &str, out: &mut Vec<String>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for k in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                changes(x.get(k).unwrap_or(&Value::Null), y.get(k).unwrap_or(&Value::Null), &format!("{path}.{k}"), out);
            }
        }
        _ if a != b => out.push(path.trim_start_matches('.').to_string()),
        _ => {}
    }
}

fn ptr(v: &Value, path: &str) -> Value {
    v.pointer(&format!("/{}", path.replace('.', "/"))).cloned().unwrap_or(Value::Null)
}

#[component]
pub fn Service() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let services = RwSignal::new(Vec::<Value>::new());
    let current = Memo::new(move |_| params.get().get("name").unwrap_or_default());
    let view_ = RwSignal::new(Value::Null); // get_service result
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let yaml_edit = RwSignal::new(None::<String>);
    let err = RwSignal::new(String::new());
    // "+ New service" links to ?new=1: open on it (also when already here), and drop it on close so the next click opens again.
    let qm_new = leptos_router::hooks::use_query_map();
    let adding = RwSignal::new(false);
    Effect::new(move |_| if qm_new.get().get("new").is_some() { adding.set(true) });
    let nav_new = use_navigate();
    Effect::new(move |prev: Option<bool>| {
        let open = adding.get();
        if prev == Some(true) && !open && qm_new.get_untracked().get("new").is_some() {
            nav_new(&web_sys::window().and_then(|w| w.location().pathname().ok()).unwrap_or_default(), leptos_router::NavigateOptions { replace: true, ..Default::default() });
        }
        open
    });
    let kept = RwSignal::new(0usize);
    let test_result = RwSignal::new(Value::Null);
    let sending = RwSignal::new(false);
    let steps_open = RwSignal::new(false);
    let steps_text = RwSignal::new(String::new());

    let load = move |keep_draft: bool| {
        leptos::task::spawn_local(async move {
            let Ok(v) = api::get("/services").await else { return };
            let list = v["services"].as_array().cloned().unwrap_or_default();
            services.set(list.clone());
            let name = current.get_untracked();
            let pick = list.iter().find(|x| s(&x["service"], "name") == name).or(list.first()).cloned().unwrap_or(Value::Null);
            let dirty = draft.get_untracked() != saved.get_untracked();
            view_.set(pick.clone());
            saved.set(pick["service"].clone());
            if !(keep_draft && dirty) {
                draft.set(pick["service"].clone());
            }
            if let Ok(i) = api::get("/inboxes?kind=kept").await {
                kept.set(i["inboxes"].as_array().map(|a| a.len()).unwrap_or(0));
            }
        });
    };
    Effect::new(move |_| {
        current.track();
        load(false);
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "email.service" {
            load(true);
        }
        t
    });

    let dirty = Memo::new(move |_| {
        let mut out = vec![];
        changes(&saved.get(), &draft.get(), "", &mut out);
        out
    });
    let set = move |path: &'static str, v: Value| {
        draft.update(|d| {
            if let Some(slot) = d.pointer_mut(&format!("/{}", path.replace('.', "/"))) {
                *slot = v;
            }
        });
    };
    let val = move |path: &'static str| ptr(&draft.get(), path);
    let changed = move |path: &'static str| dirty.get().iter().any(|d| d == path || d.starts_with(&format!("{path}.")));
    let name = move || s(&saved.get(), "name");

    let save = move |_| {
        let n = name();
        let body = json!({"patch": draft.get_untracked()});
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/services/{n}"), body).await {
                Ok(v) => {
                    saved.set(v["service"].clone());
                    draft.set(v["service"].clone());
                    view_.set(v);
                    err.set(String::new());
                    ui::toast("Saved");
                }
                Err(e) => err.set(e),
            }
        });
    };
    let access = move |intake: &'static str, v: String| {
        let n = name();
        let (mode, how) = v.split_once(':').map(|(a, b)| (a.to_string(), Some(b.to_string()))).unwrap_or((v, None));
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/services/{n}/intake/{intake}/access"), json!({"mode": mode, "how": how})).await {
                Ok(_) => load(true),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let smtp_opt = move |body: Value| {
        let n = name();
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/services/{n}/intake/smtp/access"), body).await {
                Ok(_) => load(true),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let send_test = move |_| {
        let n = name();
        sending.set(true);
        leptos::task::spawn_local(async move {
            let r = api::send("POST", &format!("/services/{n}/events/test"), json!({"type": "delivered"})).await;
            sending.set(false);
            match r {
                Ok(v) => test_result.set(v),
                Err(e) => test_result.set(json!({"error": e})),
            }
        });
    };

    let header_right = move || {
        view! {
            {move || {
                let d = dirty.get().len();
                if d == 0 { view! { <span class="small cap">"All changes saved"</span> }.into_any() }
                else { view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> }.into_any() }
            }}
            <button class="btn primary" disabled=move || dirty.get().is_empty() on:click=save>"Save"</button>
        }
    };

    view! {
        <div class="page">
            <ServicesHead active="services" right=header_right/>
            <div class="body">
                <div class="pane pad form">
                    {move || {
                        if draft.get().is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let vw = view_.get();
                        let sv = saved.get();
                        view! {
                            <ServiceTitle saved=saved services=services on_changed={ let nav = nav.clone(); move |n: String| { load(false); nav(&format!("/services/email/s/{n}"), Default::default()) } }/>
                            <div class="block" style="padding-top:8px">
                                <div class="row"><h3 style="margin:0">"How your app sends"</h3><span class="grow"></span><span class="caps">"Who can send"</span></div>
                                <div class="intake" style="margin-top:12px">
                                    <Toggle on=Signal::derive(move || val("intake.sendgridApi.on") == true) set=move |v: bool| set("intake.sendgridApi.on", json!(v))/>
                                    <div><div style="font-weight:500">"SendGrid API"</div>
                                        <div class="row small"><span class="mono t2">{s(&vw, "sendgridUrl")}</span><button class="link plain" on:click={ let u = s(&vw, "sendgridUrl"); move |_| ui::copy(&u, "URL") }>"Copy"</button></div></div>
                                    <AccessSelect acc=sv["intake"]["sendgridApi"].clone() smtp=false on_pick=move |v: String| access("sendgridApi", v)/>
                                </div>
                                <Keys service=name() intake="sendgridApi" acc=sv["intake"]["sendgridApi"].clone() on_change=move |_| load(true)/>
                                <div class="intake">
                                    <Toggle on=Signal::derive(move || val("intake.smtp.on") == true) set=move |v: bool| set("intake.smtp.on", json!(v))/>
                                    <div><div style="font-weight:500">"SMTP"</div>
                                        <div class="row small"><span class="mono t2">{format!("{}  ·  {}", s(&vw, "smtpAddress"), if sv["intake"]["smtp"]["starttls"] == true { "STARTTLS" } else { "no STARTTLS" })}</span>
                                            <button class="link plain" on:click={ let u = s(&vw, "smtpAddress"); move |_| ui::copy(&u, "Address") }>"Copy"</button></div>
                                        {(!s(&vw, "smtpError").is_empty()).then(|| view! { <div class="err">{format!("Not listening: {}", s(&vw, "smtpError"))}</div> })}
                                        {(!s(&vw, "smtpUser").is_empty()).then(|| {
                                            // Several environments share this port: the SMTP login names the environment.
                                            let others = vw["smtpSharedWith"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>();
                                            let user = s(&vw, "smtpUser");
                                            let (host, port) = s(&vw, "smtpAddress").rsplit_once(':').map(|(h, p)| (h.to_string(), p.to_string())).unwrap_or_default();
                                            let lines = format!("Smtp__Host={host}\nSmtp__Port={port}\nSmtp__User={user}\nSmtp__Password=any");
                                            view! {
                                                <div class="small" style="margin-top:6px"><span class="chip">{format!("shared by {} environments", others.len())}</span>
                                                    <span class="t2" style="margin-left:8px">{format!("{}: log in with the SMTP user {user}", others.join(", "))}</span></div>
                                                <div class="small cap" style="margin-top:4px">"The password is anything, or one of this service's keys when keys are checked. Without a login the server answers 530 and says what to send."</div>
                                                <pre class="mono small" style="margin:8px 0 0;padding:8px 10px;background:var(--subtle);border-radius:6px">{lines.clone()}</pre>
                                                <button class="link plain small" on:click=move |_| ui::copy(&lines, "App settings")>"Copy"</button>
                                            }
                                        })}
                                        <div class="row small" style="margin-top:6px">
                                            <label class="check"><input type="checkbox" prop:checked=sv["intake"]["smtp"]["starttls"] == true on:change=move |e| smtp_opt(json!({"starttls": event_target_checked(&e)}))/>"Offer STARTTLS"</label>
                                            <span class="sep"></span><span class="t2">"Recipients"</span>
                                            <select class="select" style="height:26px;font-size:12px" on:change=move |e| smtp_opt(json!({"refuseRecipients": event_target_value(&e)}))>
                                                {[("none", "accepted"), ("550", "refused 550"), ("451", "refused 451, try later")].into_iter().map(|(v, l)| view! { <option value=v selected=s(&sv["intake"]["smtp"], "refuseRecipients") == v>{l}</option> }).collect_view()}
                                            </select>
                                        </div>
                                        <div class="field" style="margin-top:6px;flex-direction:row;align-items:center"><span class="small t2">"Port"</span>
                                            <input class="input mono" class:changed=move || changed("intake.smtp.port") style="width:90px;height:28px" type="number" prop:value=move || val("intake.smtp.port").to_string()
                                                on:input=move |e| set("intake.smtp.port", json!(event_target_value(&e).parse::<u16>().unwrap_or(2525)))/></div>
                                    </div>
                                    <AccessSelect acc=sv["intake"]["smtp"].clone() smtp=true on_pick=move |v: String| access("smtp", v)/>
                                </div>
                                <Keys service=name() intake="smtp" acc=sv["intake"]["smtp"].clone() on_change=move |_| load(true)/>
                                <div class="small t2" style="padding-left:50px">"Another sending API (Mailgun, Amazon SES, Postmark) comes as its own plugin from the Catalog."</div>
                            </div>

                            <div class="block">
                                <h3>"Addresses"</h3>
                                <div class="grid2">
                                    <div class="field"><label>"Domain for temporary inboxes"</label>
                                        <input class="input mono" class:changed=move || changed("addresses.domain") prop:value=move || s(&val("addresses"), "domain") on:input=move |e| set("addresses.domain", json!(event_target_value(&e)))/></div>
                                    <div class="field"><label>"Temporary inboxes expire after"</label>
                                        <select class="select" class:changed=move || changed("addresses.temporaryTtl") style="width:160px" on:change=move |e| set("addresses.temporaryTtl", json!(event_target_value(&e)))>
                                            {[("15m", "15 min"), ("30m", "30 min"), ("1h", "1 hour"), ("2h", "2 hours"), ("24h", "24 hours")].into_iter().map(|(v, l)| view! { <option value=v selected=move || s(&val("addresses"), "temporaryTtl") == v>{l}</option> }).collect_view()}
                                        </select></div>
                                </div>
                                <div class="row" style="margin-top:14px">
                                    <Toggle on=Signal::derive(move || val("addresses.catchAll") == true) set=move |v: bool| set("addresses.catchAll", json!(v))/>
                                    <span>"Catch mail to any other domain"</span><span class="t2">"·  nothing ever reaches a real inbox"</span>
                                </div>
                                <div class="small t2" style="margin:8px 0 0 42px">{move || format!("{} inboxes kept with {}  ·  ", kept.get(), api::s(&env.get(), "env").to_uppercase())}<a class="link" href="/inboxes">"Manage in Inboxes →"</a></div>
                                {move || (val("addresses.catchAll") == false).then(|| view! { <div class="small attn" style="margin:6px 0 0 42px">"Off: SMTP refuses other domains at RCPT TO (550) and the SendGrid API records them as dropped."</div> })}
                            </div>

                            <div class="block">
                                <h3>"Events your app receives"</h3>
                                <div class="field"><label>"Send them to"</label>
                                    <div class="row">
                                        <input class="input mono grow" class:changed=move || changed("events.to") class:bad=move || { let t = s(&val("events"), "to"); !t.is_empty() && !t.starts_with("http://") && !t.starts_with("https://") }
                                            placeholder="https://your-app/hooks/sendgrid" prop:value=move || s(&val("events"), "to") on:input=move |e| set("events.to", json!(event_target_value(&e)))/>
                                        <select class="select" style="width:150px"><option>"SendGrid format"</option></select>
                                    </div>
                                    {move || { let t = s(&val("events"), "to"); (!t.is_empty() && !t.starts_with("http")).then(|| view! { <div class="err">"Start with https:// or http://"</div> }) }}
                                </div>
                                <div class="row wrap" style="margin-top:10px;gap:14px">
                                    {[("processed", "processed"), ("delivered", "delivered"), ("deferred", "deferred"), ("bounce", "bounce"), ("dropped", "dropped"), ("open", "open"), ("click", "click"), ("spamreport", "spam report"), ("unsubscribe", "unsubscribe")].into_iter().map(|(t, l)| view! {
                                        <label class="check"><input type="checkbox" prop:checked=move || val("events.types").as_array().map(|a| a.iter().any(|x| x == t)).unwrap_or(false)
                                            on:change=move |_| draft.update(|d| { let a = d["events"]["types"].as_array_mut().unwrap(); if let Some(i) = a.iter().position(|x| x == t) { a.remove(i); } else { a.push(json!(t)); } })/>{l}</label>
                                    }).collect_view()}
                                </div>
                                {move || val("events.types").as_array().map(|a| a.is_empty()).unwrap_or(true).then(|| view! { <div class="small attn" style="margin-top:6px">"⚠ no event type checked: your app will get no events"</div> })}
                                <div class="row" style="margin-top:14px">
                                    <Toggle on=Signal::derive(move || s(&val("events"), "sign") == "ecdsa") set=move |v: bool| set("events.sign", json!(if v { "ecdsa" } else { "none" }))/>
                                    <span>"Sign them the way SendGrid does (ECDSA)"</span><span class="grow"></span>
                                    <button class="link plain" on:click={ let k = s(&vw, "publicKey"); move |_| ui::copy(&k, "Public key") }>"Copy public key"</button>
                                </div>
                                <div class="row small" style="margin:8px 0 0 42px">
                                    <span class="t2">"Inboxes without their own steps:"</span>
                                    <span class="mono" class:attn=move || changed("events.defaultSteps")>{move || val("events.defaultSteps").as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).filter(|x| !x.is_empty()).unwrap_or("none".into())}</span>
                                    <span class="grow"></span>
                                    <button class="link plain" on:click=move |_| { steps_text.set(val("events.defaultSteps").as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()); steps_open.set(true) }>"Edit steps"</button>
                                </div>
                                <div class="row" style="margin-top:16px">
                                    <button class="btn" disabled=move || sending.get() || s(&saved.get()["events"], "to").is_empty() on:click=send_test>{move || if sending.get() { "Sending…" } else { "Send test event" }}</button>
                                    {move || {
                                        let r = test_result.get();
                                        let lt = saved.get()["events"]["lastTest"].clone();
                                        if !r.is_null() {
                                            let rep = &r["reply"];
                                            if !s(&r, "error").is_empty() { view! { <span class="small danger">{s(&r, "error")}</span> }.into_any() }
                                            else if rep["status"].as_u64().map(|c| c < 300).unwrap_or(false) { view! { <span class="small ok">{format!("✓ your app replied {} in {} ms", rep["status"], rep["ms"])}</span> }.into_any() }
                                            else if !rep["status"].is_null() { view! { <span class="small danger">{format!("✕ your app replied {} in {} ms  ·  {}", rep["status"], rep["ms"], s(rep, "body").chars().take(80).collect::<String>())}</span> }.into_any() }
                                            else { view! { <span class="small danger">{format!("✕ {}  ·  is the URL reachable from the simulator?", s(rep, "error"))}</span> }.into_any() }
                                        } else if !lt.is_null() {
                                            view! { <span class="small t2">{format!("Last test {}  ·  {}", api::hm(api::n(&lt, "at")), if lt["status"].is_null() { s(&lt, "error") } else { format!("your app replied {} in {} ms", lt["status"], lt["ms"]) })}</span> }.into_any()
                                        } else if s(&saved.get()["events"], "to").is_empty() {
                                            view! { <span class="small cap">"Save a URL first."</span> }.into_any()
                                        } else { ().into_any() }
                                    }}
                                </div>
                            </div>

                            <div class="block">
                                <h3>"Limits"</h3>
                                <div class="grid4">
                                    <div class="field"><label>"Message size"</label><input class="input mono" class:changed=move || changed("limits.messageSize") prop:value=move || s(&val("limits"), "messageSize") on:input=move |e| set("limits.messageSize", json!(event_target_value(&e)))/></div>
                                    <div class="field"><label>"Send rate"</label><input class="input mono" class:changed=move || changed("limits.rate") prop:value=move || s(&val("limits"), "rate") on:input=move |e| set("limits.rate", json!(event_target_value(&e)))/><span class="small cap">"then 429"</span></div>
                                    <div class="field"><label>"Keep messages"</label>
                                        <select class="select" class:changed=move || changed("limits.keep") on:change=move |e| set("limits.keep", json!(event_target_value(&e)))>
                                            {[("1d", "1 day"), ("2d", "2 days"), ("7d", "7 days"), ("30d", "30 days")].into_iter().map(|(v, l)| view! { <option value=v selected=move || s(&val("limits"), "keep") == v>{l}</option> }).collect_view()}
                                        </select></div>
                                    <div class="field"><label>"Store at most"</label><input class="input mono" class:changed=move || changed("limits.max") type="number" prop:value=move || val("limits.max").to_string() on:input=move |e| set("limits.max", json!(event_target_value(&e).parse::<u64>().unwrap_or(10000)))/></div>
                                </div>
                            </div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                        }.into_any()
                    }}
                </div>
                <div class="side yaml">
                    <div class="row"><span class="mono" style="font-weight:500">{move || format!("{}.yaml", name())}</span><span class="grow"></span>
                        {move || if yaml_edit.get().is_some() {
                            view! { <button class="link plain" on:click=move |_| yaml_edit.set(None)>"Cancel"</button>
                                <button class="link" on:click=move |_| {
                                    let y = yaml_edit.get_untracked().unwrap_or_default();
                                    let n = name();
                                    leptos::task::spawn_local(async move {
                                        match api::send("PUT", &format!("/services/{n}"), json!({"yaml": y})).await {
                                            Ok(_) => { yaml_edit.set(None); load(false); ui::toast("YAML saved"); }
                                            Err(e) => ui::toast_err(e),
                                        }
                                    });
                                }>"Save YAML"</button> }.into_any()
                        } else {
                            view! { <button class="link plain" style="font-size:13px" on:click=move |_| yaml_edit.set(Some(s(&view_.get_untracked(), "yaml")))>"Edit as YAML"</button> }.into_any()
                        }}
                    </div>
                    {move || match yaml_edit.get() {
                        Some(y) => view! { <textarea class="yaml" style="margin-top:12px" prop:value=y on:input=move |e| yaml_edit.set(Some(event_target_value(&e)))></textarea> }.into_any(),
                        None => {
                            let d = dirty.get();
                            view! {
                                <pre class="yaml">{s(&view_.get(), "yaml")}</pre>
                                {(!d.is_empty()).then(|| view! { <div class="small attn" style="margin-top:10px">{format!("Changed, not saved yet: {}", d.join(", "))}</div> })}
                            }.into_any()
                        }
                    }}
                    <div class="small cap" style="margin-top:12px">{move || format!("File: {}", s(&view_.get(), "file"))}</div>
                </div>
            </div>
            <AddService open=adding services=services/>
            <Dialog open=steps_open title="Default steps">
                <div class="small t2">"For inboxes without their own steps and for the catch-all. One step per line, the same vocabulary as Inboxes, the YAML and MCP."</div>
                <textarea class="yaml" style="min-height:140px;margin-top:10px" prop:value=move || steps_text.get() on:input=move |e| steps_text.set(event_target_value(&e))></textarea>
                <div class="small cap" style="margin-top:8px">"delivered +1s  ·  opened +30s ±5s  ·  click:signLink +500ms  ·  opened +2m from step:1  ·  spamreport +1h"</div>
                <div class="actions">
                    <button class="btn" on:click=move |_| steps_open.set(false)>"Cancel"</button>
                    <button class="btn primary" on:click=move |_| { set("events.defaultSteps", json!(steps_text.get_untracked().lines().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>())); steps_open.set(false); }>"Use these steps"</button>
                </div>
                <div class="small cap">"They are saved with the service (Save at the top); a step the server does not understand is refused with the reason."</div>
            </Dialog>
        </div>
    }
}

#[component]
fn ServiceTitle(saved: RwSignal<Value>, services: RwSignal<Vec<Value>>, #[prop(into)] on_changed: Callback<String>) -> impl IntoView {
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let menu = RwSignal::new(false);
    let rename = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let remove = RwSignal::new(false);
    let err = RwSignal::new(String::new());
    let name = move || s(&saved.get(), "name");
    let patch = move |body: Value| {
        let n = name();
        leptos::task::spawn_local(async move {
            match api::send("PATCH", &format!("/services/{n}"), body).await {
                Ok(v) => {
                    rename.set(false);
                    on_changed.run(s(&v["service"], "name"));
                }
                Err(e) => err.set(e),
            }
        });
    };
    let do_remove = move |_| {
        let n = name();
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/services/{n}"), json!({})).await {
                Ok(_) => {
                    ui::toast(format!("{n} removed"));
                    nav("/services/email", Default::default());
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="mhead">
            <div class="row" style="align-items:baseline">
                <h2 style="margin:0">{name}</h2>
                <span class="small cap">{move || { let d = saved.get()["default"] == true; format!("{}plugin {}", if d { format!("default for {}  ·  ", api::s(&env.get(), "env").to_uppercase()) } else { String::new() }, s(&saved.get(), "plugin")) }}</span>
            </div>
            <div class="anchor">
                <button class="btn icon" on:click=move |_| menu.set(true)>"⋯"</button>
                <Pop open=menu right=true>
                    {move || (saved.get()["default"] != true).then(|| view! { <div class="mi" on:click=move |_| patch(json!({"default": true}))>{move || format!("Make default for {}", api::s(&env.get(), "env").to_uppercase())}</div> })}
                    <div class="mi" on:click=move |_| { new_name.set(name()); err.set(String::new()); rename.set(true) }>"Rename"</div>
                    <div class="rule"></div>
                    <div class="mi danger" on:click=move |_| remove.set(true)>"Remove service…"</div>
                </Pop>
            </div>
        </div>
        <Dialog open=rename title="Rename service">
            <input class="input mono" style="width:100%" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e))/>
            <div class="small cap" style="margin-top:6px">"Lowercase letters, digits and dashes. The file is renamed too."</div>
            {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
            <div class="actions"><button class="btn" on:click=move |_| rename.set(false)>"Cancel"</button>
                <button class="btn primary" on:click=move |_| patch(json!({"newName": new_name.get_untracked()}))>"Rename"</button></div>
        </Dialog>
        <Confirm open=remove question=Signal::derive(move || format!("Remove {}?", name()))
            detail=Signal::derive(move || format!("Its messages stay until their retention ends. {}", if services.get().len() <= 1 { "This is the only e-mail service: apps sending here will be refused." } else { "" }))
            action="Remove" on_yes=do_remove/>
    }
}

/// "Any API key works ▾" / Validate keys / Don't connect ▸ how.
#[component]
fn AccessSelect(acc: Value, smtp: bool, #[prop(into)] on_pick: Callback<String>) -> impl IntoView {
    let mode = s(&acc, "mode");
    let how = s(&acc, "how");
    let cur = if mode == "refuse" { format!("refuse:{how}") } else { mode.clone() };
    let any = if smtp { "Any login works" } else { "Any API key works" };
    let by = s(&acc, "setBy");
    let provoked = mode == "refuse";
    let desc = |h: &str| match (h, smtp) {
        ("timeout", _) => "times out after 30 s",
        ("refused", _) => "refuses the connection",
        ("tls", _) => "fails the TLS handshake",
        ("503", true) => "answers 421 service not available",
        ("503", false) => "answers 503",
        ("429", true) => "answers 421 too many connections",
        _ => "answers 429",
    };
    view! {
        <div>
            <select class="select" style="width:100%" on:change=move |e| on_pick.run(event_target_value(&e))>
                <option value="any" selected=cur == "any">{any}</option>
                <option value="validate" selected=cur == "validate">"Validate keys"</option>
                <optgroup label="Don't connect">
                    {["timeout", "refused", "tls", "503", "429"].into_iter().map(|h| { let v = format!("refuse:{h}"); view! { <option value=v.clone() selected=cur == v>{desc(h)}</option> } }).collect_view()}
                </optgroup>
            </select>
            {provoked.then(|| view! { <div class="small provoked" style="margin-top:4px">{format!("{}{}", desc(&how), if by.is_empty() { String::new() } else { format!("  ·  set by {} {}", by.trim_start_matches("mcp:").trim_start_matches("api:"), api::ago(api::n(&acc, "setAt"))) })}</div> })}
        </div>
    }
}

#[component]
fn Keys(service: String, intake: &'static str, acc: Value, #[prop(into)] on_change: Callback<()>) -> impl IntoView {
    let validating = s(&acc, "mode") == "validate";
    let keys = acc["keys"].as_array().cloned().unwrap_or_default();
    let shown = RwSignal::new(None::<String>);
    let label = RwSignal::new(String::new());
    let gen_open = RwSignal::new(false);
    let svc = StoredValue::new(service);
    let generate = move |_| {
        let n = svc.get_value();
        let l = label.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/services/{n}/intake/{intake}/keys"), json!({"label": if l.is_empty() { "key".to_string() } else { l }})).await {
                Ok(v) => {
                    shown.set(Some(s(&v, "secret")));
                    on_change.run(());
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let revoke = move |id: String| {
        let n = svc.get_value();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/services/{n}/intake/{intake}/keys/{id}"), json!({})).await {
                Ok(_) => on_change.run(()),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        {(validating || !keys.is_empty()).then(|| view! {
            <div style="margin:-6px 0 14px 50px">
                <div class="caps" style="margin-bottom:4px">{if validating { "Keys  ·  only these can send" } else { "Keys  ·  used when you validate" }}</div>
                {keys.into_iter().map(|k| { let id = s(&k, "id"); view! {
                    <div class="keyrow"><span class="mono">{if intake == "smtp" { format!("••••••••{}", s(&k, "hint")) } else { format!("SG.••••••••{}", s(&k, "hint")) }}</span>
                        <span class="t2">{s(&k, "label")}</span><span class="cap">{format!("created {}", api::ago(api::n(&k, "created")))}</span>
                        <button class="link danger" on:click=move |_| revoke(id.clone())>"Revoke"</button></div>
                } }).collect_view()}
            </div>
        })}
        <div style="margin:-8px 0 14px 50px"><button class="link plain" on:click=move |_| { shown.set(None); label.set(String::new()); gen_open.set(true) }>"+  Generate key"</button></div>
        <Dialog open=gen_open title="Generate key">
            <div class="fd narrow">
                <div class="small t2">{move || format!("For {}  ·  {}. Shown once.", svc.get_value(), if intake == "smtp" { "SMTP" } else { "SendGrid API" })}</div>
                {move || match shown.get() {
                    None => view! {
                        <div class="caps fd-l">"Who will use it"</div>
                        <input class="input" style="width:100%" placeholder="api-staging" prop:value=move || label.get() on:input=move |e| label.set(event_target_value(&e))/>
                        <div class="caps fd-l">"The app sends it as"</div>
                        <div class="fd-now mono small" style="margin-top:0">{if intake == "smtp" { "SMTP AUTH: user apikey, password <key>" } else { "Authorization: Bearer <key>" }}</div>
                        <div class="small" style="margin-top:10px" class:attn=!validating class:cap=validating>{if validating { "This service checks keys: sends without a valid key get 401 (535 on SMTP)." } else { "This service checks keys: off, so any key works until you turn it on above." }}</div>
                        <div class="fd-foot"><span class="grow"></span><button class="btn" on:click=move |_| gen_open.set(false)>"Cancel"</button><button class="btn primary" on:click=generate>"Generate"</button></div>
                    }.into_any(),
                    Some(secret) => { let s2 = secret.clone(); view! {
                        <div class="caps fd-l">"The key"</div>
                        <div class="copybox"><span class="ell">{secret.clone()}</span><button class="link" on:click=move |_| ui::copy(&s2, "Key")>"Copy"</button></div>
                        <div class="caps fd-l">"The app sends it as"</div>
                        <div class="fd-now mono small" style="margin-top:0">{if intake == "smtp" { format!("SMTP AUTH: user apikey, password {secret}") } else { format!("Authorization: Bearer {secret}") }}</div>
                        <div class="small t2" style="margin-top:10px">"Copy it into the app's configuration now; Stuntman does not show it again."</div>
                        <div class="fd-foot"><span class="grow"></span><button class="btn primary" on:click=move |_| gen_open.set(false)>"Done"</button></div>
                    }.into_any() }
                }}
            </div>
        </Dialog>
    }
}

#[component]
fn AddService(open: RwSignal<bool>, services: RwSignal<Vec<Value>>) -> impl IntoView {
    let nav = use_navigate();
    let env = expect_context::<RwSignal<Value>>();
    let name = RwSignal::new(String::new());
    let copy = RwSignal::new(String::new());
    let err = RwSignal::new(String::new());
    let add = move |_| {
        let (n, c) = (name.get_untracked(), copy.get_untracked());
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", "/services", json!({"channel": "email", "name": n, "copyFrom": if c.is_empty() { None } else { Some(c) }})).await {
                Ok(v) => {
                    open.set(false);
                    nav(&format!("/services/email/s/{}", s(&v["service"], "name")), Default::default());
                }
                Err(e) => err.set(e),
            }
        });
    };
    view! {
        <Dialog open=open title="Add e-mail service">
            <div class="fd narrow">
                <div class="small t2">{move || format!("One more SMTP server and SendGrid API in {}.", s(&env.get(), "env").to_uppercase())}</div>
                <div class="caps fd-l">"Name"</div>
                <input class="input mono" style="width:100%" placeholder="billing" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
                <div class="caps fd-l">"Start from"</div>
                <div class="fd-cards one">
                    <button class="fd-card" class:on=move || copy.get().is_empty() on:click=move |_| copy.set(String::new())><span class="fd-radio"></span><b>"Blank"</b>
                        <span class="small t2">"SMTP on, any login; SendGrid API on; no inbox steps"</span></button>
                    {move || services.get().into_iter().map(|v| { let n = s(&v["service"], "name"); let (n2, n3) = (n.clone(), n.clone()); view! {
                        <button class="fd-card" class:on=move || copy.get() == n2 on:click=move |_| copy.set(n3.clone())><span class="fd-radio"></span><b>{format!("Copy {n}")}</b>
                            <span class="small t2">"its logins, keys, steps and extractors"</span></button>
                    } }).collect_view()}
                </div>
                <div class="fd-now mono small">
                    <div>"SMTP      localhost:<next free port>"</div>
                    <div>{format!("SendGrid  {}{}/v3/mail/send", web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default(), api::url(""))}</div>
                </div>
                {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:8px">{err.get()}</div> })}
                <div class="fd-foot">
                    <span class="small attn grow">{move || { let n = name.get().trim().to_string(); if services.get().iter().any(|v| s(&v["service"], "name") == n) { format!("{n} already exists") } else { String::new() } }}</span>
                    <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button><button class="btn primary" on:click={ let add = add.clone(); move |e| add(e) }>"Add service"</button>
                </div>
            </div>
        </Dialog>
    }
}
