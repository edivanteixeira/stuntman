//! A16 · Mailboxes: the boxes the app reads over IMAP and POP3. Messages waiting (with their
//! attachments and flags), what the app did (commands, without passwords), a message put "as the
//! supplier sent it", failures provoked.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
pub fn Mailboxes() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let all = RwSignal::new(Value::Null);
    let box_ = RwSignal::new(Value::Null);
    let (from, subject, text, att_name, att_text) = (RwSignal::new("billing@globex.example".to_string()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let new_addr = RwSignal::new(String::new());
    let cur = Memo::new(move |_| qm.get().get("m").or_else(|| all.get()["mailboxes"].as_array().and_then(|a| a.first().map(|x| s(x, "address")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let c = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/mailboxes").await { all.set(v) }
            if !c.is_empty() { if let Ok(v) = api::get(&format!("/mailboxes/{}/messages", enc(&c))).await { box_.set(v) } }
        });
    });
    let put = move |_| {
        let c = cur.get_untracked();
        let mut body = json!({"from": from.get_untracked(), "subject": subject.get_untracked(), "text": text.get_untracked()});
        if !att_name.get_untracked().is_empty() { body["attachments"] = json!([{"filename": att_name.get_untracked(), "content_type": if att_name.get_untracked().ends_with(".xml") { "application/xml" } else { "application/octet-stream" }, "text": att_text.get_untracked()}]); }
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/mailboxes/{}/messages", enc(&c)), body).await { Ok(_) => ui::toast("In the mailbox: the app finds it on its next poll"), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |k: &'static str| { let c = cur.get_untracked(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/mailboxes/{}", enc(&c)), json!({"provoke": k})).await { Ok(_) => ui::toast(if k == "none" { "Back to normal" } else { "Provoked" }), Err(e) => ui::toast_err(e) } }) };
    let create = move |_| { let a = new_addr.get_untracked(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/mailboxes/{}", enc(&a)), json!({})).await { Ok(_) => ui::toast("Mailbox created"), Err(e) => ui::toast_err(e) } }) };
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Mailboxes"</div><div class="small cap">"IMAP · POP3 · the app reads"</div></div>
                {move || all.get()["mailboxes"].as_array().cloned().unwrap_or_default().into_iter().map(|b| { let a = s(&b, "address"); let a2 = a.clone(); view! {
                    <a class="mrow" class:on=move || cur.get() == a2 href=format!("/mailboxes?m={}", enc(&a))><div class="row"><span class="ell">{s(&b, "address")}</span><span class="grow"></span><span class="mono small cap">{n(&b, "messages").to_string()}</span></div>
                        <div class="small cap">{format!("{} unread{}", n(&b, "unread"), if s(&b, "provoke").is_empty() { String::new() } else { format!(" · provoked: {}", s(&b, "provoke")) })}</div></a> } }).collect_view()}
                <div class="stack" style="padding:12px 20px"><input class="input" placeholder="invoices@acme.example" prop:value=move || new_addr.get() on:input=move |e| new_addr.set(event_target_value(&e))/><button class="btn small" on:click=create>"+ New mailbox"</button></div>
            </div>
            <div class="page">
                <AreaHead kind=Signal::derive(move || format!("E-mail  ·  mailbox the app reads  ·  {}  ·  {}", s(&all.get()["connect"], "imap"), s(&all.get()["connect"], "pop3"))) title=Signal::derive(move || cur.get())
                    tabs=vec![("messages", "Messages", "/notifications".into()), ("mailboxes", "Mailboxes", "/mailboxes".into()), ("inboxes", "Inboxes", "/inboxes".into())]
                    counts=Signal::derive(move || vec![("mailboxes", all.get()["mailboxes"].as_array().map(|a| a.len()).unwrap_or(0).to_string())]) active="mailboxes"/>
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    <table class="tbl"><tr><th></th><th>"From"</th><th>"Subject"</th><th>"Attachments"</th><th>"When"</th></tr>
                        {move || box_.get()["messages"].as_array().cloned().unwrap_or_default().into_iter().map(|m| view! { <tr>
                            <td class="link">{if m["seen"].as_bool().unwrap_or(false) { "" } else { "●" }}</td><td>{s(&m, "from")}</td><td>{s(&m, "subject")}{m["deleted"].as_bool().unwrap_or(false).then(|| " (deleted)")}</td>
                            <td class="mono small">{m["attachments"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default()}</td><td class="mono">{api::ago(n(&m, "at"))}</td></tr> }).collect_view()}</table>
                    <div class="caps" style="margin-top:10px">"Put a message here · as a supplier would send it"</div>
                    <div class="stack" style="max-width:680px"><div class="grid2"><input class="input" placeholder="from" prop:value=move || from.get() on:input=move |e| from.set(event_target_value(&e))/>
                        <input class="input" placeholder="subject (Invoice 1234)" prop:value=move || subject.get() on:input=move |e| subject.set(event_target_value(&e))/></div>
                        <textarea class="input" rows="2" placeholder="text" prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))></textarea>
                        <div class="grid2"><input class="input mono" placeholder="attachment name (invoice.xml)" prop:value=move || att_name.get() on:input=move |e| att_name.set(event_target_value(&e))/>
                            <input class="input mono" placeholder="attachment content (<invoice …/>)" prop:value=move || att_text.get() on:input=move |e| att_text.set(event_target_value(&e))/></div>
                        <div><button class="btn primary small" on:click=put>"Put the message"</button></div></div>
                    <div class="caps" style="margin-top:10px">"What the app did"</div>
                    <pre class="yaml" style="white-space:pre-wrap;max-height:220px;overflow:auto">{move || box_.get()["log"].as_array().cloned().unwrap_or_default().into_iter().rev().map(|l| format!("{}  {:<9} {}  {}\n", api::hms(n(&l, "at")), s(&l, "proto"), s(&l, "line"), s(&l, "result"))).collect::<String>()}</pre>
                    <div class="caps">"Provoke"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">{[("login_refused", "Login refused"), ("busy", "Mailbox in use"), ("drop_fetch", "Drop during FETCH"), ("tls_required", "TLS required"), ("token_expired", "XOAUTH2 token expired"), ("none", "Normal")].into_iter().map(|(k, l)| view! {
                        <button class="btn small" on:click=move |_| provoke(k)>{l}</button> }).collect_view()}</div>
                </div>
            </div>
        </div>
    }
}
