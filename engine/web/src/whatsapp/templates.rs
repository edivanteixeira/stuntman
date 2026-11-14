//! W2 · Templates: the list (status by colour), what the app asked for that is not declared, the
//! template rendered per language, its variables with the values the app sent last, and on the
//! right the status Stuntman keeps (instead of Meta's review), quality, today's use and the YAML.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::Head;
use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

fn st_class(st: &str) -> &'static str {
    match st { "approved" => "ok", "rejected" | "disabled" => "danger", _ => "attn" }
}

#[component]
pub fn Templates() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let lang = RwSignal::new(String::new());
    let new_open = RwSignal::new(false);
    let form = RwSignal::new(json!({"name": "", "language": "en_US", "header": "", "body": "", "footer": "", "buttons": [], "category": "utility", "status": "approved"}));
    let samples = RwSignal::new(Vec::<String>::new());
    let yaml_open = RwSignal::new(false);
    let yaml = RwSignal::new(String::new());
    let num = super::number();
    let load = move || leptos::task::spawn_local(async move { if let Ok(v) = api::get("/whatsapp/templates").await { data.set(v); } });
    load();
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("whatsapp.") { load(); } t });
    let list = Memo::new(move |_| data.get()["templates"].as_array().cloned().unwrap_or_default());
    let exists = move |n: &str| !n.is_empty() && list.get().iter().any(|x| s(&x["template"], "name") == n || s(x, "name") == n);
    let cur = Memo::new(move |_| {
        let l = list.get();
        qm.get().get("name").and_then(|n| l.iter().find(|x| s(&x["template"], "name") == n).cloned()).or_else(|| l.first().cloned()).unwrap_or(Value::Null)
    });
    Effect::new(move |_| { let c = cur.get(); lang.set(c["template"]["languages"][0].as_str().unwrap_or("").to_string()); });
    let set_status = move |name: String, st: String, reason: String| leptos::task::spawn_local(async move {
        match api::send("PUT", &format!("/whatsapp/templates/{}/status", enc(&name)), json!({"status": st, "reason": reason})).await {
            Ok(_) => ui::toast(format!("{name} is {st}; message_template_status_update sent to the app")), Err(e) => ui::toast_err(e) }
    });
    let patch = move |name: String, body: Value, msg: &'static str| leptos::task::spawn_local(async move {
        match api::send("PATCH", &format!("/whatsapp/templates/{}", enc(&name)), body).await { Ok(_) => ui::toast(msg), Err(e) => ui::toast_err(e) }
    });
    let add = move |_| {
        let mut f = form.get_untracked();
        // Empty buttons and optional parts are left out of the file.
        if let Some(b) = f["buttons"].as_array_mut() { b.retain(|x| !s(x, "text").trim().is_empty()); }
        for k in ["header", "footer"] { if s(&f, k).trim().is_empty() { f.as_object_mut().map(|o| o.remove(k)); } }
        leptos::task::spawn_local(async move {
            match api::send("POST", "/whatsapp/templates", f.clone()).await {
                Ok(_) => { new_open.set(false); ui::toast(format!("{} added to whatsapp/templates.yaml", s(&f, "name"))); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let open_new = move |prefill: Value| { form.set(prefill); samples.set(vec![]); new_open.set(true); };
    view! {
        <div class="page">
            <Head active="templates" right=move || view! {
                <button class="btn" on:click=move |_| { yaml_open.set(true); leptos::task::spawn_local(async move { yaml.set(reqwasm_text("/api/whatsapp/files/templates.yaml").await); }); }>"View YAML"</button>
                <button class="btn primary" on:click=move |_| open_new(json!({"name": "", "language": "en_US", "header": "", "body": "", "footer": "", "buttons": [], "category": "utility", "status": "approved"}))>"New template"</button>
            }/>
            <div class="body">
                <div class="list" style="width:368px">
                    {move || list.get().into_iter().map(|x| {
                        let t = x["template"].clone();
                        let n = s(&t, "name");
                        let (n1, n2) = (n.clone(), n.clone());
                        let st = s(&t, "status");
                        let nav = nav.clone();
                        view! { <div class="mrow" class:on=move || s(&cur.get()["template"], "name") == n1 on:click=move |_| nav(&format!("/whatsapp/templates?name={}", enc(&n2)), Default::default())>
                            <div class="row"><span class="mono grow">{n.clone()}</span><span class=format!("stl {}", st_class(&st))>{st.to_uppercase()}</span></div>
                            <div class="small cap">{format!("{}  ·  {}", s(&t, "category"), t["languages"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap_or("")).collect::<Vec<_>>().join(", ")).unwrap_or_default())}</div>
                        </div> }
                    }).collect_view()}
                    {move || { let u = data.get()["undeclared"].as_array().cloned().unwrap_or_default(); (!u.is_empty()).then(|| view! {
                        <div class="caps section" style="padding:18px 24px 8px">"Asked for but not declared"</div>
                        {u.into_iter().map(|x| {
                            let pre = json!({"name": s(&x, "name"), "language": s(&x, "language"), "header": "", "category": "utility", "status": "approved",
                                "body": (1..=api::n(&x, "params")).map(|i| format!("{{{{{i}}}}}")).collect::<Vec<_>>().join(" ")});
                            view! { <div class="mrow"><div class="row"><span class="mono danger grow">{s(&x, "name")}</span><button class="link" on:click=move |_| open_new(pre.clone())>"Add"</button></div>
                                <div class="small cap">{format!("{}  ·  asked {} time(s), answered 132001  ·  {} parameter(s)", s(&x, "language"), api::n(&x, "count"), api::n(&x, "params"))}</div></div> }
                        }).collect_view()}
                    }) }}
                    {move || list.get().is_empty().then(|| view! { <div class="callout" style="margin:20px"><div class="t">"No templates yet"</div><div class="t2">"Add the ones your app sends, or let it create them through Meta's API (they start pending)."</div></div> })}
                    <div class="small cap" style="padding:14px 24px">"Templates live in whatsapp/templates.yaml. Status is Stuntman's: no review by Meta happens here."</div>
                </div>
                <div class="pane pad">
                    {move || {
                        let x = cur.get();
                        if x.is_null() { return view! { <div></div> }.into_any(); }
                        let t = x["template"].clone();
                        let u = x["usage"].clone();
                        let vals: Vec<String> = u["lastParams"].as_array().cloned().unwrap_or_default().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
                        let vars: Vec<String> = t["variables"].as_array().cloned().unwrap_or_default().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
                        let np = api::n(&x, "params");
                        let langs: Vec<String> = t["languages"].as_array().cloned().unwrap_or_default().iter().map(|v| v.as_str().unwrap_or("").to_string()).collect();
                        let btns = t["buttons"].as_array().cloned().unwrap_or_default();
                        view! {
                            <div class="mono" style="font-size:18px">{s(&t, "name")}</div>
                            <div class="small cap">{format!("{}  ·  {}body with {} variable(s){}{}", s(&t, "category"), if s(&t, "header").is_empty() { "" } else { "header text, " }, np,
                                if s(&t, "footer").is_empty() { String::new() } else { ", footer".into() }, if btns.is_empty() { String::new() } else { format!(", {} button(s)", btns.len()) })}</div>
                            <div class="tabs" style="height:30px;margin:8px 0 14px">{langs.into_iter().map(|l| { let (l1, l2) = (l.clone(), l.clone()); view! { <div class="tab" class:on=move || lang.get() == l1 on:click=move |_| lang.set(l2.clone())>{l}</div> } }).collect_view()}</div>
                            <div class="wa-preview"><div class="bubble out">
                                <div class="tbar"></div>
                                {(!s(&t, "header").is_empty()).then(|| view! { <div class="strong">{s(&t, "header")}</div> })}
                                <div class="body-t">{s(&t, "body")}</div>
                                {(!s(&t, "footer").is_empty()).then(|| view! { <div class="small cap">{s(&t, "footer")}</div> })}
                                {btns.iter().map(|b| view! { <div class="wa-btn">{if s(b, "url").is_empty() { "↩ " } else { "↗ " }}{s(b, "text")}</div> }).collect_view()}
                            </div></div>
                            <div class="small cap" style="margin-top:8px">{btns.iter().map(|b| if s(b, "url").is_empty() { format!("quick reply: {}", s(b, "quick_reply")) } else { format!("URL button: {}", s(b, "url")) }).collect::<Vec<_>>().join("  ·  ")}</div>
                            <div class="caps" style="margin:22px 0 8px">{match u["lastAt"].as_i64() { Some(at) => format!("Variables  ·  last sent by {} at {}", s(&u, "lastBy"), api::hm(at)), None => "Variables".into() }}</div>
                            <table class="kv2">{(1..=np).map(|i| view! { <tr><td class="mono">{format!("{{{{{i}}}}}")}</td><td>{vars.get(i as usize - 1).cloned().unwrap_or_default()}</td><td class="mono">{vals.get(i as usize - 1).cloned().unwrap_or("—".into())}</td></tr> }).collect_view()}</table>
                            <div class="small cap" style="margin-top:10px">{format!("Stuntman checks what the app sends: {} parameters instead of {np} is answered 132000 “Number of parameters does not match”, a missing language 132001.", if np > 1 { np - 1 } else { np + 1 })}</div>
                        }.into_any()
                    }}
                </div>
                <div class="side envside">
                    {move || {
                        let x = cur.get();
                        if x.is_null() { return view! { <div></div> }.into_any(); }
                        let t = x["template"].clone();
                        let u = x["usage"].clone();
                        let name = s(&t, "name");
                        let (n1, n2, n3, n4) = (name.clone(), name.clone(), name.clone(), name.clone());
                        let st = s(&t, "status");
                        let reason = RwSignal::new(if s(&t, "reason").is_empty() { "INVALID_FORMAT".to_string() } else { s(&t, "reason") });
                        let yaml_t = serde_yaml_like(&t);
                        view! {
                            <div class="caps">"Status in Stuntman"</div>
                            <select class="input" style="margin:6px 0" on:change=move |e| set_status(n1.clone(), event_target_value(&e), reason.get_untracked())>
                                {[("approved", "Approved"), ("pending", "Pending review"), ("rejected", "Rejected"), ("paused", "Paused (quality)"), ("disabled", "Disabled")].into_iter().map(|(v, l)| view! { <option value=v selected=st == v>{l}</option> }).collect_view()}
                            </select>
                            {(st == "rejected").then(|| { let n = n4.clone(); view! { <div class="row small"><span class="cap">"Reason"</span>
                                <select class="input" on:change=move |e| { reason.set(event_target_value(&e)); set_status(n.clone(), "rejected".into(), reason.get_untracked()) }>
                                    {["INVALID_FORMAT", "ABUSIVE_CONTENT", "TAG_CONTENT_MISMATCH", "SCAM", "INCORRECT_CATEGORY"].into_iter().map(|r| view! { <option value=r selected=move || reason.get() == r>{r}</option> }).collect_view()}</select></div> } })}
                            {(st == "pending").then(|| { let (a, b) = (n2.clone(), n2.clone()); let api_made = !t["created_at"].is_null(); view! { <div class="row" style="gap:8px;margin:6px 0">
                                <button class="btn" on:click=move |_| set_status(a.clone(), "approved".into(), String::new())>"Approve now"</button>
                                <button class="btn" on:click=move |_| set_status(b.clone(), "rejected".into(), "INVALID_FORMAT".into())>"Reject"</button></div>
                                <div class="small cap">{move || format!("Until approved, sends are refused (132001). {}", if api_made { format!("Created by the app: approves by itself after {} (Number settings).", s(&num.get()["number"], "approve_after")) } else { "Declared as pending: approve it here.".to_string() })}</div> } })}
                            {(st == "rejected").then(|| { let n = name.clone(); view! { <div class="small cap" style="margin:6px 0">"Sends by the app are refused with 132001; it should fall back (SMS, e-mail) or stop sending it."</div>
                                <a class="link small" href=format!("/whatsapp?q={}", enc(&n))>"Messages refused ›"</a> } })}
                            {(st == "paused").then(|| { let a = n2.clone(); view! { <div class="small cap" style="margin:6px 0">"Paused, as Meta does after many blocks or reports: sends get 132015 “Template is paused”."</div>
                                <button class="btn" style="margin:2px 0 6px" on:click=move |_| set_status(a.clone(), "approved".into(), String::new())>"Unpause"</button> } })}
                            <div class="small cap">"Changing it sends message_template_status_update to the app, and sends now follow the new status."</div>
                            <div class="caps" style="margin-top:20px">"Quality"</div>
                            <select class="input" style="margin:6px 0" on:change=move |e| patch(n3.clone(), json!({"quality": event_target_value(&e)}), "Quality changed")>
                                {[("high", "High"), ("medium", "Medium"), ("low", "Low")].into_iter().map(|(v, l)| view! { <option value=v selected=s(&t, "quality") == v>{l}</option> }).collect_view()}
                            </select>
                            <div class="small cap">"Low quality pauses the template, as Meta does."</div>
                            <div class="caps" style="margin-top:20px">"Category"</div>
                            <select class="input" style="margin:6px 0" on:change={ let n = name.clone(); move |e| patch(n.clone(), json!({"category": event_target_value(&e)}), "Category changed; template_category_update sent") }>
                                {["utility", "marketing", "authentication"].into_iter().map(|v| view! { <option value=v selected=s(&t, "category") == v>{v}</option> }).collect_view()}
                            </select>
                            <div class="caps" style="margin-top:20px">{move || format!("Use in {}", s(&env.get(), "env").to_uppercase())}</div>
                            <table class="kv2">
                                <tr><td>"Sent today"</td><td class="mono">{api::n(&u, "sent")}</td></tr>
                                <tr><td>"Failed"</td><td class="mono danger">{api::n(&u, "failed")}</td></tr>
                                <tr><td>"Read"</td><td class="mono">{api::n(&u, "read")}</td></tr>
                                <tr><td>"Buttons tapped"</td><td class="mono">{api::n(&u, "buttonsTapped")}</td></tr>
                            </table>
                            <a class="link small" href=format!("/whatsapp?q={}", enc(&name))>"Messages with this template ›"</a>
                            <div class="caps" style="margin-top:20px">"whatsapp/templates.yaml"</div>
                            <pre class="yaml envyaml">{yaml_t}</pre>
                        }.into_any()
                    }}
                </div>
            </div>
            <Dialog open=new_open title="New template">
                <div class="fd wide">
                    <div class="small t2">"Templates the app may send from this number; sample values only fill the preview."</div>
                    <div class="fd-cols">
                        <div>
                            <div class="row" style="gap:12px">
                                <div class="grow"><div class="caps fd-l">"Name"</div><input class="input mono" style="width:100%" placeholder="signature_request" prop:value=move || s(&form.get(), "name") on:input=move |e| form.update(|f| f["name"] = json!(event_target_value(&e).to_lowercase().replace([' ', '-'], "_")))/></div>
                                <div style="width:160px"><div class="caps fd-l">"Language"</div><select class="select" style="width:100%" on:change=move |e| form.update(|f| f["language"] = json!(event_target_value(&e)))>
                                    {["en_US", "en_GB", "es_ES", "es_MX", "pt_BR"].into_iter().map(|v| view! { <option value=v selected=move || s(&form.get(), "language") == v>{v}</option> }).collect_view()}</select></div>
                            </div>
                            <div class="caps fd-l">"Category"</div>
                            <ui::Seg options=vec![("utility", "Utility"), ("marketing", "Marketing"), ("authentication", "Authentication")] value=Signal::derive(move || s(&form.get(), "category")) set=Callback::new(move |v: String| form.update(|f| f["category"] = json!(v)))/>
                            <div class="caps fd-l">"Header (optional)"</div>
                            <input class="input" style="width:100%" placeholder="Signature pending" prop:value=move || s(&form.get(), "header") on:input=move |e| form.update(|f| f["header"] = json!(event_target_value(&e)))/>
                            <div class="caps fd-l">"Body"</div>
                            <textarea class="input" style="width:100%;min-height:76px;padding:8px 10px" placeholder="Hi {{1}}, {{2}} sent you {{3}} to sign by {{4}}." prop:value=move || s(&form.get(), "body") on:input=move |e| form.update(|f| f["body"] = json!(event_target_value(&e)))></textarea>
                            {move || { let n = vars(&s(&form.get(), "body")); (n > 0).then(|| view! {
                                <div class="caps fd-l">"Sample values"</div>
                                <div class="row" style="gap:8px;flex-wrap:wrap">{(1..=n).map(|i| view! {
                                    <input class="input mono" style="width:120px" placeholder=format!("{{{{{i}}}}}") prop:value=move || samples.get().get(i - 1).cloned().unwrap_or_default()
                                        on:input=move |e| { let v = event_target_value(&e); samples.update(|s| { if s.len() < i { s.resize(i, String::new()); } s[i - 1] = v; }) }/> }).collect_view()}</div> }) }}
                            <div class="caps fd-l">"Footer (optional)"</div>
                            <input class="input" style="width:100%" placeholder="Acme Corp" prop:value=move || s(&form.get(), "footer") on:input=move |e| form.update(|f| f["footer"] = json!(event_target_value(&e)))/>
                            <div class="caps fd-l">"Buttons"</div>
                            {move || form.get()["buttons"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, b)| { let is_url = b.get("url").is_some(); view! {
                                <div class="row" style="gap:8px;margin-bottom:6px">
                                    <select class="select" style="width:120px" on:change=move |e| { let k = event_target_value(&e); form.update(|f| { let t = s(&f["buttons"][i], "text"); f["buttons"][i] = if k == "url" { json!({"text": t, "url": ""}) } else { json!({"text": t, "quick_reply": true}) }; }) }>
                                        <option value="url" selected=is_url>"URL"</option><option value="quick" selected=!is_url>"Quick reply"</option></select>
                                    <input class="input" style="width:150px" placeholder="Sign now" prop:value=s(&b, "text") on:input=move |e| form.update(|f| f["buttons"][i]["text"] = json!(event_target_value(&e)))/>
                                    {is_url.then(|| view! { <input class="input mono grow" placeholder="https://acme.example.com/s/{{1}}" prop:value=s(&b, "url") on:input=move |e| form.update(|f| f["buttons"][i]["url"] = json!(event_target_value(&e)))/> })}
                                    <button class="link plain" on:click=move |_| form.update(|f| { if let Some(a) = f["buttons"].as_array_mut() { a.remove(i); } })>"×"</button>
                                </div> } }).collect_view()}
                            {move || (form.get()["buttons"].as_array().map(|a| a.len()).unwrap_or(0) < 3).then(|| view! {
                                <button class="link" on:click=move |_| form.update(|f| { if !f["buttons"].is_array() { f["buttons"] = json!([]); } f["buttons"].as_array_mut().unwrap().push(json!({"text": "", "url": ""})); })>"+ Quick reply or URL"</button> })}
                        </div>
                        <div>
                            <div class="caps fd-l">"Preview"</div>
                            <div class="wa-prev"><div class="bub">
                                {move || { let h = s(&form.get(), "header"); (!h.trim().is_empty()).then(|| view! { <div style="font-weight:600;margin-bottom:6px">{fill(&h, &samples.get())}</div> }) }}
                                <div style="white-space:pre-wrap">{move || { let b = s(&form.get(), "body"); if b.trim().is_empty() { "The body shows here.".to_string() } else { fill(&b, &samples.get()) } }}</div>
                                {move || { let f = s(&form.get(), "footer"); (!f.trim().is_empty()).then(|| view! { <div class="small cap" style="margin-top:8px">{f}</div> }) }}
                                {move || form.get()["buttons"].as_array().cloned().unwrap_or_default().into_iter().filter(|b| !s(b, "text").trim().is_empty()).map(|b| view! {
                                    <div class="btnrow">{format!("{}{}", if b.get("url").is_some() { "↗  " } else { "↩  " }, s(&b, "text"))}</div> }).collect_view()}
                            </div></div>
                            <div class="caps fd-l">"Status"</div>
                            <ui::Seg options=vec![("approved", "Approved"), ("pending", "Pending review")] value=Signal::derive(move || s(&form.get(), "status")) set=Callback::new(move |v: String| form.update(|f| f["status"] = json!(v)))/>
                            <div class="small cap" style="margin-top:6px">"Pending approves after the delay in Number settings."</div>
                        </div>
                    </div>
                    <div class="fd-foot">
                        <span class="small grow" class:attn=move || exists(&s(&form.get(), "name")) class:cap=move || !exists(&s(&form.get(), "name"))>{move || if exists(&s(&form.get(), "name")) { format!("{} exists: this adds the language to it.", s(&form.get(), "name")) } else { "Saved to whatsapp/templates.yaml".to_string() }}</span>
                        <button class="btn" on:click=move |_| new_open.set(false)>"Cancel"</button>
                        <button class="btn primary" disabled=move || s(&form.get(), "name").is_empty() || s(&form.get(), "body").is_empty() on:click=add>"Add template"</button>
                    </div>
                </div>
            </Dialog>
            <Dialog open=yaml_open title="whatsapp/templates.yaml"><pre class="yaml envyaml" style="max-height:70vh;overflow:auto">{move || yaml.get()}</pre></Dialog>
        </div>
    }
}

/// The template as its YAML block (for the right column).
fn serde_yaml_like(t: &Value) -> String {
    let mut o = format!("- name: {}\n  category: {}\n  languages: [{}]\n  status: {}\n  quality: {}\n", s(t, "name"), s(t, "category"),
        t["languages"].as_array().map(|a| a.iter().map(|l| l.as_str().unwrap_or("")).collect::<Vec<_>>().join(", ")).unwrap_or_default(), s(t, "status"), s(t, "quality"));
    if !s(t, "header").is_empty() { o += &format!("  header: {}\n", s(t, "header")); }
    o += &format!("  body: \"{}\"\n", s(t, "body"));
    if let Some(b) = t["buttons"].as_array().filter(|b| !b.is_empty()) {
        o += "  buttons:\n";
        for x in b { o += &if s(x, "url").is_empty() { format!("    - quick_reply: {}\n", s(x, "quick_reply")) } else { format!("    - url: {}\n", s(x, "url")) }; }
    }
    o
}

pub async fn reqwasm_text(path: &str) -> String {
    match gloo_net::http::Request::get(&api::url(path)).send().await { Ok(r) => r.text().await.unwrap_or_default(), Err(_) => String::new() }
}

/// How many {{n}} variables a body uses (the highest n).
fn vars(body: &str) -> usize {
    body.split("{{").skip(1).filter_map(|p| p.split("}}").next()?.trim().parse::<usize>().ok()).max().unwrap_or(0)
}

/// The text with each {{n}} replaced by its sample (or left as {{n}}).
fn fill(text: &str, samples: &[String]) -> String {
    let mut out = text.to_string();
    for (i, v) in samples.iter().enumerate() {
        if !v.is_empty() { out = out.replace(&format!("{{{{{}}}}}", i + 1), v); }
    }
    out
}
