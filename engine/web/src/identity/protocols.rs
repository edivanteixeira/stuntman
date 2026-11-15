//! A14 · Protocols: the provider as a SAML 2.0 IdP, an LDAP directory, the password grant (ROPC),
//! and the Bot Framework around a Teams bot.

use leptos::prelude::*;
use serde_json::{json, Value};

use super::{fetch, sel, Head};
use crate::{api::{self, enc, s}, ui};

#[component]
pub fn Protocols() -> impl IntoView {
    let p = sel();
    let data = fetch("tenant", p);
    let (endpoint, bot, text) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let twist = RwSignal::new(String::new());
    let set = move |body: Value, msg: &'static str| { let n = p.get_untracked(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/identity/providers/{}", enc(&n)), body).await { Ok(_) => ui::toast(msg), Err(e) => ui::toast_err(e) } }) };
    let send_bot = move |_| {
        let body = json!({"endpoint": endpoint.get_untracked(), "bot_app_id": bot.get_untracked(), "text": text.get_untracked(), "twist": twist.get_untracked()});
        leptos::task::spawn_local(async move { match api::send("POST", "/identity/bot/activities", body).await { Ok(r) => ui::toast(format!("The bot answered {}", api::n(&r, "status"))), Err(e) => ui::toast_err(e) } });
    };
    let block = move |cap: &'static str, on: bool, text: String, addr: String, toggle: Value| view! {
        <div class="section" style="border-bottom:1px solid var(--rule);padding:14px 0">
            <div class="row"><span class="caps">{cap}</span><span class="grow"></span>
                <ui::Toggle on=Signal::derive(move || on) set=Callback::new(move |v: bool| { let mut b = toggle.clone(); for (_, x) in b.as_object_mut().unwrap().iter_mut() { *x = json!(v) } set(b, "Saved") })/></div>
            <div class="small t2" style="margin-top:6px;white-space:pre-line">{text}</div>
            {(!addr.is_empty()).then(|| view! { <div class="mono small" style="margin-top:6px">{addr}</div> })}
        </div>
    };
    view! {
        <div class="page">
            <Head active="protocols" right=|| ()/>
            <div class="body" style="flex-direction:column;overflow:auto;padding:8px 24px">
                {move || { let d = data.get(); (!d.is_null()).then(|| { let sm = d["saml"].clone(); let ld = d["ldap"].clone(); view! {
                    {block("SAML 2.0 identity provider", !sm.is_null(), "Metadata, SSO by redirect and POST, signed assertions (RSA-SHA256), the person picked on the sign-in page.\nProvoke from Sign-ins: expired, bad_signature, wrong_audience, user_cancels.".into(),
                        if sm.is_null() { String::new() } else { format!("metadata: {}   ·   SSO: {}   ·   entity: {}", s(&sm, "metadata"), s(&sm, "sso"), s(&sm, "entityId")) }, json!({"saml": true}))}
                    {block("LDAP directory", !ld.is_null(), "Simple bind (DN, user@domain, DOMAIN\\user), search and compare on the people of this tenant; groups as memberOf.\nWrong password answers 49 (data 52e); locked users data 775; disabled data 533.".into(),
                        if ld.is_null() { String::new() } else { format!("{}   ·   base dn: {}   ·   bind: uid=<user>,ou=people,{}", s(&ld, "url"), s(&ld, "baseDn"), s(&ld, "baseDn")) }, json!({"ldap": true}))}
                    {(!ld.is_null()).then(|| view! { <div class="row" style="gap:6px;margin:8px 0">{[("normal", "Normal"), ("busy", "Busy (51)"), ("unavailable", "Unavailable (52)"), ("timeout", "Timeout")].into_iter().map(|(k, l)| { let cur = s(&ld, "provoke"); view! {
                        <button class="btn small" class:primary=(cur == k) || (cur.is_empty() && k == "normal") on:click=move |_| set(json!({"ldap_provoke": k}), "LDAP provocation set")>{l}</button> } }).collect_view()}</div> })}
                    {block("Password grant (ROPC)", d["passwordGrant"].as_bool().unwrap_or(false), "grant_type=password on the token endpoint, for apps that still send a user and a password.\nMFA users get AADSTS50076; a wrong password AADSTS50126; a locked user AADSTS50053.".into(),
                        format!("POST {}   grant_type=password", s(&d, "tokenEndpoint")), json!({"password_grant": true}))}
                    <div class="section" style="padding:14px 0">
                        <div class="caps">"Bot Framework"</div>
                        <div class="small t2" style="margin-top:6px">"The OpenID metadata and keys the Bot Connector signs with, and activities sent to the app's messaging endpoint as Teams would, signed. The bot's replies land on the connector below."</div>
                        <div class="mono small" style="margin-top:6px">{format!("openid: {}   ·   connector (serviceUrl): {}", s(&d["botframework"], "openid"), s(&d["botframework"], "connector"))}</div>
                        <div class="stack" style="max-width:720px;margin-top:10px"><div class="grid2">
                            <input class="input mono" placeholder="the bot's messaging endpoint" prop:value=move || endpoint.get() on:input=move |e| endpoint.set(event_target_value(&e))/>
                            <input class="input mono" placeholder="the bot's app id (JWT audience)" prop:value=move || bot.get() on:input=move |e| bot.set(event_target_value(&e))/></div>
                            <input class="input" placeholder="message text" prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))/>
                            <div class="row" style="gap:6px">{[("", "Signed"), ("bad_signature", "Bad signature"), ("expired", "Expired"), ("wrong_audience", "Wrong audience"), ("no_token", "No token")].into_iter().map(|(k, l)| view! {
                                <button class="btn small" class:primary=move || twist.get() == k on:click=move |_| twist.set(k.to_string())>{l}</button> }).collect_view()}
                                <span class="grow"></span><button class="btn primary small" on:click=send_bot>"Send to the bot"</button></div></div>
                    </div>
                } }) }}
            </div>
        </div>
    }
}
