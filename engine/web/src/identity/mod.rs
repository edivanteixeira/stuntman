//! Identity / SSO (I1–I5): Sign-ins · Users · Apps · Tenant settings of an identity provider (Entra ID
//! stand-in first). The provider comes from ?provider= (else the first one).

pub mod apps;
pub mod clients;
pub mod signins;
pub mod tenant;
pub mod users;

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

/// Providers (with counts), refreshed on live updates.
pub fn providers() -> RwSignal<Value> {
    let live = expect_context::<api::Live>();
    let v = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(x) = api::get("/identity/providers").await { v.set(x); } });
    });
    v
}

pub fn current(all: &Value, q: Option<String>) -> Value {
    let list = all["providers"].as_array().cloned().unwrap_or_default();
    q.and_then(|n| list.iter().find(|x| s(x, "name") == n).cloned()).or_else(|| list.first().cloned()).unwrap_or(Value::Null)
}

/// The provider in the URL (it wins even before the list has it), else the first one.
pub fn sel() -> Memo<String> {
    let qm = use_query_map();
    let all = providers();
    Memo::new(move |_| qm.get().get("provider").filter(|v| !v.is_empty()).unwrap_or_else(|| s(&current(&all.get(), None), "name")))
}

pub fn href(tab: &str, provider: &str, extra: &str) -> String {
    let base = match tab { "users" => "/identity/users", "apps" => "/identity/apps", "tenant" => "/identity/tenant", _ => "/identity" };
    format!("{base}?provider={}{}", enc(provider), extra)
}

/// One fetch of a provider path, refreshed when the provider or anything live changes.
pub fn fetch(path: &'static str, provider: Memo<String>) -> RwSignal<Value> {
    let live = expect_context::<api::Live>();
    let v = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let p = provider.get();
        if p.is_empty() { v.set(Value::Null); return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/identity/{}/{path}", enc(&p))).await { if provider.get_untracked() == p { v.set(x); } }
        });
    });
    v
}

#[component]
pub fn Head(active: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let env = expect_context::<RwSignal<Value>>();
    let all = providers();
    let p = sel();
    let cur = Memo::new(move |_| current(&all.get(), Some(p.get())));
    let tabs: [(&'static str, &'static str); 4] = [("signins", "Sign-ins"), ("users", "Users"), ("apps", "Apps"), ("tenant", "Tenant settings")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("Identity  ·  {}  ·  {}", { let l = s(&cur.get(), "label"); if l.is_empty() { "Identity provider".to_string() } else { l } }, s(&env.get(), "env").to_uppercase())}</div>
                    <h1>{move || { let c = cur.get(); if c.is_null() { "Identity".to_string() } else { format!("{}  ·  {}", s(&c, "label"), s(&c, "tenant")) } }}</h1>
                </div>
                <span class="grow"></span>
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label)| {
                    let nav = nav.clone();
                    view! {
                        <div class="tab" class:on=active == key on:click=move |_| nav(&href(key, &p.get_untracked(), ""), Default::default())>
                            {label}<span class="n">{move || match key { "signins" => api::n(&cur.get(), "signInsLastHour").to_string(), "users" => api::n(&cur.get(), "users").to_string(), "apps" => api::n(&cur.get(), "apps").to_string(), _ => String::new() }}</span></div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

/// No provider yet: what it is and how to make one.
#[component]
pub fn NoProvider() -> impl IntoView {
    let open = RwSignal::new(false);
    view! {
        <div class="pane pad"><div class="callout"><div class="t">"No identity provider in this environment"</div>
            <div class="t2">"A provider answers like Microsoft Entra ID (or Google, Okta, any OIDC) with synthetic people and app registrations. The app sets its Authority to it and signs people in through Stuntman's login page."</div>
            <button class="btn primary" on:click=move |_| open.set(true)>"New provider"</button></div></div>
        <NewProvider open=open/>
    }
}

/// New provider: id, flavour, tenant.
#[component]
pub fn NewProvider(open: RwSignal<bool>) -> impl IntoView {
    let (name, tenant, flavour) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new("entra-v2".to_string()));
    let create = move |_| {
        let (n, t, f) = (name.get_untracked().trim().to_string(), tenant.get_untracked().trim().to_string(), flavour.get_untracked());
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            let mut b = json!({"flavour": f});
            if !t.is_empty() { b["tenant"] = json!(t); }
            match api::send("PUT", &format!("/identity/providers/{}", enc(&n)), b).await {
                Ok(_) => { open.set(false); ui::toast(format!("Provider {n} created")); nav(&href("users", &n, ""), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <Dialog open=open title="New identity provider">
            <div class="fd">
                <div class="small t2">"Synthetic people, app registrations and a login page that answers like the provider. Apps point their Authority at it."</div>
                <div class="caps fd-l">"Answers like"</div>
                <div class="fd-cards two">
                    {[("entra-v2", "Microsoft Entra ID", "v2.0 endpoints, AADSTS errors, oid/tid, groups overage"), ("google", "Google", "accounts.google.com shape, hd and email_verified"), ("okta", "Okta", "org authorization server, groups claim"), ("oidc", "Any OIDC provider", "the standard claims only")]
                        .into_iter().map(|(id, t, d)| view! { <button class="fd-card" class:on=move || flavour.get() == id on:click=move |_| flavour.set(id.into())><span class="fd-radio"></span><b>{t}</b><span class="small t2">{d}</span></button> }).collect_view()}
                </div>
                <div class="row" style="gap:12px">
                    <div class="grow"><div class="caps fd-l">"Provider id"</div><input class="input mono" style="width:100%" placeholder="entra" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/></div>
                    <div class="grow"><div class="caps fd-l">"Tenant"</div><input class="input mono" style="width:100%" placeholder="contoso-stg" prop:value=move || tenant.get() on:input=move |e| tenant.set(event_target_value(&e))/></div>
                </div>
                <div class="fd-foot"><span class="small cap grow">{move || format!("Authority: …/login/{}/v2.0", if tenant.get().trim().is_empty() { "<tenant>".to_string() } else { tenant.get().trim().to_string() })}</span>
                    <button class="btn" on:click=move |_| open.set(false)>"Cancel"</button><button class="btn primary" on:click=create>"Create provider"</button></div>
            </div>
        </Dialog>
    }
}

/// Explanation of an error code in the provider's words (I1/I2/I4 states).
pub fn code_note(code: &str) -> &'static str {
    match code {
        "AADSTS50020" => "The app is single tenant. The error page and the redirect with error= are the provider's; the app should explain, not loop.",
        "AADSTS50076" => "MFA code wrong three times: the user is locked for 5 min, like the provider does.",
        "access_denied" => "The app gets error=access_denied&error_description=… at its redirect URI.",
        "AADSTS50057" => "Shown on the login page; the app is never called back.",
        "AADSTS50011" => "Exact match only, as the provider does. Add the URI to the app, or fix the app's setting.",
        "AADSTS7000215" => "Wrong client secret at POST /token. Turn \"Accept any client secret\" off to test it.",
        "AADSTS7000222" => "The client secret expired. Rotate it in Secrets; the app should read the new one without a redeploy.",
        "AADSTS54005" => "The app redeemed the same code twice (a retry, or two tabs).",
        "AADSTS70043" | "AADSTS700082" => "Refresh tokens rotate on use and expire; the app must keep the newest one.",
        "AADSTS65001" => "Consent was revoked or never given: the next sign-in shows the consent page again.",
        "AADSTS90033" => "The provider is down (provoked). Apps should show a clear message and let the person retry.",
        _ => "",
    }
}

pub fn result_class(r: &Value) -> &'static str {
    let code = s(r, "code");
    if code.is_empty() { if s(r, "result") == "signed in" || s(r, "result") == "refreshed" || s(r, "result") == "issued" { "ok" } else { "cap" } }
    else if code == "access_denied" { "attn" } else { "danger" }
}
