//! A5 · Settings › MCP access: the sign-in this server uses (read from its STUNTMAN_AUTH_* settings)
//! and the people's agents signed in to the central MCP, each one revocable.

use leptos::prelude::*;
use serde_json::{json, Value};

use crate::{
    api::{self, s},
    ui,
};

fn when(ms: i64) -> String {
    if ms <= 0 { return "—".into(); }
    let d = js_sys::Date::new(&(ms as f64).into());
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{} {:02} {}", M[d.get_month() as usize], d.get_date(), api::hm(ms))
}

#[component]
pub fn Access() -> impl IntoView {
    let data = RwSignal::new(Value::Null);
    let load = move || leptos::task::spawn_local(async move {
        match api::get("/mcp-access").await { Ok(v) => data.set(v), Err(e) => ui::toast_err(e) }
    });
    load();
    let revoke = move |id: String, email: String| leptos::task::spawn_local(async move {
        let path = if id.is_empty() { format!("/mcp-access/all?email={}", api::enc(&email)) } else { format!("/mcp-access/{id}") };
        match api::send("DELETE", &path, json!({})).await { Ok(v) => { ui::toast(format!("Revoked {}", api::n(&v, "revoked"))); load() } Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="page">
            <div class="head"><div class="top"><h1>"MCP access"</h1><span class="sub">"Settings · people's agents signed in to the central MCP"</span></div></div>
            <div class="body"><div class="pane pad">
                {move || {
                    let v = data.get();
                    if v.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                    let si = v["signIn"].clone();
                    if si.is_null() {
                        return view! { <div class="callout"><div class="t">"Sign-in is off"</div>
                            <div class="t2">"Anyone who reaches this server can use it. Set STUNTMAN_AUTH_ISSUER, STUNTMAN_AUTH_CLIENT_ID, STUNTMAN_AUTH_CLIENT_SECRET and STUNTMAN_AUTH_ALLOWED_DOMAINS (or _EMAILS) on the server to turn it on; see the Environments guide."</div></div> }.into_any();
                    }
                    let domains = si["allowedDomains"].as_array().cloned().unwrap_or_default().iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>();
                    let who = format!("{}{}", if domains.is_empty() { String::new() } else { format!("the domain{} {}", if domains.len() == 1 { "" } else { "s" }, domains.join(", ")) },
                        match api::n(&si, "allowedEmails") { 0 => String::new(), n => format!("{}{n} more address{}", if domains.is_empty() { "" } else { " · " }, if n == 1 { "" } else { "es" }) });
                    view! {
                        <div class="caps" style="margin-bottom:12px">"Sign-in (from the server's settings)"</div>
                        <table class="kv2">
                            <tr><td>"Provider"</td><td class="mono small">{s(&si, "provider")}</td></tr>
                            <tr><td>"Client ID"</td><td class="mono small">{s(&si, "clientId")}</td></tr>
                            <tr><td>"Who may sign in"</td><td>{who}</td></tr>
                            <tr><td>"Session"</td><td>{format!("{} hours", api::n(&si, "sessionHours"))}</td></tr>
                            <tr><td>"Admin key"</td><td>{if si["adminKey"] == true { "set (STUNTMAN_ADMIN_KEY)" } else { "not set" }}</td></tr>
                        </table>
                        <div class="small cap" style="margin:10px 0 26px">"These come from STUNTMAN_AUTH_* on the server; change them there and restart. Nothing secret is shown."</div>
                        <table class="tbl">
                            <thead><tr><th>"Person"</th><th>"Client"</th><th>"Last used"</th><th>"Expires"</th><th>"Starts in"</th><th></th></tr></thead>
                            <tbody>{v["grants"].as_array().cloned().unwrap_or_default().into_iter().map(|g| {
                                let dead = g["expired"] == true;
                                let (id, email) = (s(&g, "id"), s(&g, "email"));
                                view! { <tr class:cap=dead>
                                    <td><div style="font-weight:500">{s(&g, "name")}</div><div class="small cap">{email.clone()}</div></td>
                                    <td>{s(&g, "client")}</td><td class="cap">{when(api::n(&g, "lastUsed"))}</td>
                                    <td>{if dead { "expired".to_string() } else { when(api::n(&g, "expires")) }}</td>
                                    <td class="mono small">{if g["startsIn"].is_null() { "—".to_string() } else { s(&g, "startsIn") }}</td>
                                    <td><button class="link danger" on:click=move |_| revoke(id.clone(), String::new())>"Revoke"</button></td>
                                </tr> }
                            }).collect_view()}</tbody>
                        </table>
                        {v["grants"].as_array().map(|a| a.is_empty()).unwrap_or(true).then(|| view! { <div class="small cap" style="margin-top:12px">"No agent signed in yet."</div> })}
                        <div class="small cap" style="margin-top:18px">"Revoke ends the access and the refresh token at once: the agent's next call gets 401 and asks to sign in again. Someone taken off the list loses every token at their next use."</div>
                    }.into_any()
                }}
            </div></div>
        </div>
    }
}
