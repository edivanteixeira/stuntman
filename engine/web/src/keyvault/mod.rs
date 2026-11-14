//! Key Vault (K1–K5): the column of vaults (with the active tab's items under the open vault) beside
//! Secrets · Keys · Certificates · Access log · Settings. Same shell as Storage (C3).

pub mod certificates;
pub mod keys;
pub mod log;
pub mod secrets;
pub mod settings;

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

/// Every vault (with counts), refreshed on live updates.
pub fn vaults() -> RwSignal<Value> {
    let live = expect_context::<api::Live>();
    let v = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(x) = api::get("/keyvault/vaults").await { v.set(x); } });
    });
    v
}

/// The vault in the URL, else the first one.
pub fn current(all: &Value, q: Option<String>) -> Value {
    let list = all["vaults"].as_array().cloned().unwrap_or_default();
    q.and_then(|a| list.iter().find(|x| s(x, "name") == a).cloned()).or_else(|| list.first().cloned()).unwrap_or(Value::Null)
}

pub fn href(tab: &str, vault: &str, item: &str) -> String {
    let base = if tab == "secrets" { "/secrets".to_string() } else { format!("/secrets/{tab}") };
    let mut q = format!("?vault={}", enc(vault));
    if !item.is_empty() { q.push_str(&format!("&item={}", enc(item))); }
    format!("{base}{q}")
}

/// "Nov 10" from unix seconds.
pub fn day_s(secs: i64) -> String {
    crate::environments::day(secs * 1000)
}

/// The URL a vault named `name` gets (from the engine's template).
pub fn vault_url(all: &Value, name: &str) -> String {
    let n = name.trim();
    s(all, "urlTemplate").replace("{name}", if n.is_empty() { "<name>" } else { n })
}

/// A synthetic value to start from ("Generate").
pub fn synthetic(prefix: &str) -> String {
    let abc = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let tail: String = (0..14).map(|_| abc[(js_sys::Math::random() * abc.len() as f64) as usize % abc.len()] as char).collect();
    format!("{prefix}-{tail}")
}

/// K1–K3 "every state": what the app is getting from this item right now, as notes on top of the detail.
/// `kind` is secrets | keys | certificates. Actions: enable (disabled), renew (expired certificate), normal (stop a provocation).
pub fn state_notes(kind: &'static str, x: &Value, vault: &str, enable: Callback<()>, renew: Callback<()>, normal: Callback<()>) -> impl IntoView {
    let name = s(x, "name");
    let now = api::now() as i64 / 1000;
    let cur = x["versions"][0].clone();
    let mut out: Vec<AnyView> = vec![];
    let card = |cls: &'static str, title: String, body: String, action: Option<AnyView>| view! {
        <div class=format!("callout {cls}") style="max-width:none;margin:0 0 12px"><div class="t">{title}</div><div class="t2" style="margin:0">{body}</div>{action}</div> }.into_any();
    let log_link = |label: &str| view! { <a class="link" href=format!("/secrets/log?vault={}&item={}", enc(vault), enc(&name))>{format!("{label} ›")}</a> }.into_any();
    // A provocation in force.
    let prov = s(&x["provoked"], "answer");
    if !prov.is_empty() {
        let what = match (kind, prov.as_str()) {
            (_, "403") => "403 Forbidden", (_, "404") | ("certificates", "missing") => "404 Not found", (_, "429") => "429 Throttled", (_, "disabled") => "403 disabled",
            ("keys", "slow") => "a 2 s wait", (_, "slow") => "a 3 s wait", (_, "wrong_signature") => "a signature that fails verify", (_, "expired") => "an expired certificate",
            (_, "not_yet_valid") => "a certificate not valid yet", (_, "wrong_password") => "a .pfx that does not open with its password", (_, "untrusted") => "a certificate signed by \"Unknown Test CA\"", _ => "a provoked answer" };
        let until = if x["provoked"]["until"].is_null() { "until you change it".to_string() } else { format!("for {}", api::span(api::n(&x["provoked"], "until") - api::now() as i64)) };
        let tip = match prov.as_str() {
            "wrong_signature" => "Tests the code that verifies before trusting: the signed PDF or token must be rejected downstream.",
            "missing" | "404" => "The app should exit with the name of what is missing, once, and not restart in a loop.",
            "429" => "The app should back off for Retry-After and keep using its cached value.",
            "untrusted" => "For apps that check the chain: the leaf is valid but its issuer is not in their trust store.",
            "wrong_password" => "Tests the error the app shows when it cannot open the certificate.",
            _ => "The app should fail clearly at start-up and keep its cached value later.",
        };
        out.push(card("danger-l", format!("Next {} of {name} get {what}, {until}", if kind == "keys" { "operations" } else { "reads" }), tip.to_string(),
            Some(view! { <button class="link" style="margin-top:8px" on:click=move |_| normal.run(())>"Back to normal"</button> }.into_any())));
    }
    // Disabled / expired current version.
    if cur["enabled"] == json!(false) {
        out.push(card("attn-l", format!("{name} is disabled"), if kind == "keys" { "Operations get 403 \"Operation is not allowed on a disabled key\". The app should stop and say why, not produce unsigned files.".into() } else { "Reads get 403 \"Operation get is not allowed on a disabled secret\". Enable it, or keep it off to test the fallback path.".into() },
            Some(view! { <button class="link" style="margin-top:8px" on:click=move |_| enable.run(())>"Enable"</button> }.into_any())));
    }
    if let Some(e) = cur["exp"].as_i64().filter(|e| *e <= now) {
        out.push(card("attn-l", format!("{name} current version expired {}", api::ago(e * 1000)),
            if kind == "certificates" { "Still returned, as Azure does; signatures made with it fail validation downstream. Renew issues a new version from the Test CA.".into() } else { "Azure still returns an expired value; only the attributes say so. Stuntman does the same, so an app that checks exp can be tested both ways.".into() },
            (kind == "certificates").then(|| view! { <button class="link" style="margin-top:8px" on:click=move |_| renew.run(())>"Renew now"</button> }.into_any())));
    }
    // The latest refusal of a real call (not a provocation).
    if let Some(r) = x["refusals"].as_array().and_then(|a| a.iter().find(|r| !s(r, "why").starts_with("provoked")).cloned()) {
        let st = api::n(&r, "status");
        let why = s(&r, "why");
        let body = match st {
            401 => "Token without a known oid: a process with no identity in the vault, often a local run with someone's own token.".to_string(),
            429 => "Over the vault's throttle. Caching the value is the fix.".to_string(),
            _ if why.contains("scope") || why.contains("role") || s(&r, "wouldFail").contains("Rbac") => format!("{why}. ForbiddenByRbac, as Azure answers when the identity has no role on the item: usually a scope that misses a new name."),
            _ => why.clone(),
        };
        out.push(card("danger-l", format!("{}  ·  {} {}  ·  {st}  ·  {}", s(&r, "identity"), s(&r, "op"), s(&r, "item"), api::ago(api::n(&r, "at"))), body,
            Some(if st == 403 || st == 401 { view! { <a class="link" style="margin-top:8px;display:inline-block" href=format!("/secrets/settings?vault={}", enc(vault))>"Vault settings ›"</a> }.into_any() } else { log_link("Access log") })));
    }
    // Rotated in the last 10 minutes.
    if x["versions"].as_array().map(|a| a.len() > 1).unwrap_or(false) && now - api::n(&cur, "created") < 600 {
        out.push(card("ok-l", format!("{name}  ·  new version {}", short(&s(&cur, "id"))),
            format!("{} went out {}. The access log shows when each process picked it up.", match kind { "keys" => "KeyNewVersionCreated", "certificates" => "CertificateNewVersionCreated", _ => "SecretNewVersionCreated" }, api::ago(api::n(&cur, "created") * 1000)),
            Some(log_link("Who read the new version"))));
    }
    out.into_iter().collect_view()
}

/// After a dialog opened from ?new=<name> closes, drop ?new from the URL so the same link opens it again.
pub fn drop_new_on_close(open: RwSignal<bool>) {
    let qm = use_query_map();
    let nav = use_navigate();
    Effect::new(move |prev: Option<bool>| {
        let o = open.get();
        if prev == Some(true) && !o {
            let q = qm.get_untracked();
            if q.get("new").is_some() {
                let v = q.get("vault").unwrap_or_default();
                let path = web_sys::window().and_then(|w| w.location().pathname().ok()).unwrap_or_default();
                nav(&format!("{path}?vault={}", enc(&v)), leptos_router::NavigateOptions { replace: true, ..Default::default() });
            }
        }
        o
    });
}

/// "Nov 13, 2027" from unix seconds.
pub fn ymd(secs: i64) -> String {
    let d = js_sys::Date::new(&((secs * 1000) as f64).into());
    let m = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][d.get_month() as usize];
    format!("{m} {}, {}", d.get_date(), d.get_full_year())
}

pub fn short(id: &str) -> String {
    if id.len() > 8 { format!("{}…{}", &id[..4], &id[id.len() - 4..]) } else { id.to_string() }
}

/// The left column of K1–K3 (below the header, as the frames draw it): the vaults, the open vault's
/// items for the active tab (filtered by the header's search), the connection and Vault settings.
#[component]
pub fn VaultCol(tab: &'static str, filter: RwSignal<String>) -> impl IntoView {
    let nav = use_navigate();
    let qm = use_query_map();
    let all = vaults();
    let cur = Memo::new(move |_| s(&current(&all.get(), qm.get().get("vault")), "name"));
    let rows = RwSignal::new(Vec::<Value>::new());
    let asked = RwSignal::new(Vec::<Value>::new());
    let live = expect_context::<api::Live>();
    Effect::new(move |_| {
        live.tick.track();
        let v = cur.get();
        if v.is_empty() { rows.set(vec![]); asked.set(vec![]); return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/keyvault/{tab}?vault={}", enc(&v))).await {
                rows.set(x[tab].as_array().cloned().unwrap_or_default());
                asked.set(x["askedFor"].as_array().cloned().unwrap_or_default());
            }
        });
    });
    // X4 · New vault: name, who can read it, soft delete; the values it comes with and what to set in the app.
    let new_open = RwSignal::new(false);
    let new_name = RwSignal::new(String::new());
    let any_token = RwSignal::new(true);
    let soft = RwSignal::new("90".to_string());
    let create = move |_| {
        let n = new_name.get_untracked().trim().to_string();
        if n.is_empty() { ui::toast_err("Give the vault a name"); return; }
        let any = any_token.get_untracked();
        let days = soft.get_untracked().parse::<u64>().unwrap_or(90);
        // Only identities with roles: start from the open vault's identities.
        let from = cur.get_untracked();
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            let mut b = json!({"any_token": any, "soft_delete_days": days});
            if !any && !from.is_empty() {
                if let Ok(x) = api::get(&format!("/keyvault/vaults/{}", enc(&from))).await { b["identities"] = x["vault"]["access"]["identities"].clone(); }
            }
            match api::send("PUT", &format!("/keyvault/vaults/{}", enc(&n)), b).await {
                Ok(_) => { new_open.set(false); ui::toast(format!("Vault {n} created")); nav(&href("secrets", &n, ""), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let item_q = Memo::new(move |_| qm.get().get("item").unwrap_or_default());
    let note = match tab { "keys" => "Operations in the last hour.", "certificates" => "Days until each certificate expires.", _ => "Reads in the last hour." };
    view! {
        <div class="list narrow kvcol">
            <div class="row" style="padding:16px 20px 6px"><span class="caps grow">"Vaults"</span>
                <button class="link" on:click=move |_| { new_name.set(String::new()); any_token.set(true); soft.set("90".into()); new_open.set(true); }>"+ New vault"</button></div>
            {move || all.get()["vaults"].as_array().cloned().unwrap_or_default().into_iter().map(|v| {
                let n = s(&v, "name");
                let (n2, n3) = (n.clone(), n.clone());
                let nav = nav.clone();
                let nav2 = nav.clone();
                view! {
                    <div class="kvrow" class:on=move || cur.get() == n2 && item_q.get().is_empty() on:click=move |_| nav(&href(tab, &n3, ""), Default::default())>
                        <div>{n.clone()}</div>
                        <div class="small cap">{format!("Azure Key Vault  ·  {} secrets  ·  {} keys  ·  {} certs", api::n(&v, "secrets"), api::n(&v, "keys"), api::n(&v, "certificates"))}</div>
                    </div>
                    {move || (cur.get() == n).then(|| {
                        let nav2 = nav2.clone();
                        let f = filter.get().to_lowercase();
                        rows.get().into_iter().filter(|it| f.is_empty() || s(it, "name").contains(&f) || it["tags"].to_string().to_lowercase().contains(&f)).map(|it| {
                            let name = s(&it, "name");
                            let (nm, nm2) = (name.clone(), name.clone());
                            let nav3 = nav2.clone();
                            let v = cur.get_untracked();
                            let off = it["enabled"] == json!(false);
                            let right = if tab == "certificates" { format!("{} d", api::n(&it, "daysLeft")) } else { api::thousands(api::n(&it, "readsLastHour")) };
                            let warn = (tab == "certificates" && api::n(&it, "daysLeft") < 30) || (tab != "certificates" && api::n(&it, "readsLastHour") >= 100);
                            view! {
                                <div class="kvitem" class:on=move || item_q.get() == nm class:off=off on:click=move |_| nav3(&href(tab, &v, &nm2), Default::default())>
                                    <span class="grow mono ell">{name.clone()}</span>{it["provoked"].is_object().then(|| view! { <span class="provoked small"></span> })}
                                    <span class="mono small" class:attn=warn class:cap=!warn>{right}</span>
                                </div>
                            }
                        }).collect_view()
                    })}
                }
            }).collect_view()}
            {move || { let a = asked.get(); (!a.is_empty()).then(|| { let nav = use_navigate(); view! {
                <div class="caps" style="padding:16px 20px 4px">"Asked for, not in the vault"</div>
                {a.into_iter().map(|m| { let n = s(&m, "name"); let n2 = n.clone(); let nav = nav.clone(); view! {
                    <div class="kvasked" title=format!("{} asked {} time(s); 404 {}", s(&m, "identity"), api::n(&m, "count"), api::ago(api::n(&m, "last")))
                        on:click=move |_| nav(&format!("{}&new={}", href(tab, &cur.get_untracked(), ""), enc(&n2)), Default::default())>
                        <span class="grow mono ell danger">{n}</span><span class="mono small cap">{format!("404 ×{}", api::n(&m, "count"))}</span></div> } }).collect_view()}
                <div class="small cap" style="padding:2px 20px 0">"Click one to create it."</div>
            } }) }}
            <div class="small cap" style="padding:14px 20px">{note}</div>
            <span class="grow"></span>
            {move || {
                let c = current(&all.get(), qm.get().get("vault"));
                let url = s(&c, "url");
                let u2 = url.clone();
                let name = s(&c, "name");
                (!c.is_null()).then(|| view! {
                    <div style="padding:0 20px 18px">
                        <div class="caps">"Connection"</div>
                        <div class="mono small ell" style="margin-top:6px" title=url.clone()>{url.clone()}</div>
                        <div class="small cap">{if c["anyToken"] == json!(true) { "Same URL shape as Azure; any identity token works." } else { "Same URL shape as Azure; only the identities in Vault settings." }}</div>
                        <div style="margin-top:6px"><button class="link" on:click=move |_| ui::copy(&u2, "Vault URL")>"Copy vault URL"</button></div>
                        <div style="margin-top:8px"><a class="link" href=format!("/secrets/settings?vault={}", enc(&name))>"Vault settings ›"</a></div>
                    </div>
                })
            }}
            <Dialog open=new_open title="New vault">
                <div class="fd">
                    <div class="small t2">{move || format!("Azure Key Vault API in {}, for the Azure SDK and a managed identity.", s(&expect_context::<RwSignal<Value>>().get(), "env").to_uppercase())}</div>
                    <div class="caps fd-l">"Name"</div>
                    <input class="input mono" style="width:100%" placeholder="acme-kv-stg" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e))/>
                    <div class="mono small cap ell" style="margin-top:6px">{move || vault_url(&all.get(), &new_name.get())}</div>
                    <div class="caps fd-l">"Who can read it"</div>
                    <div class="fd-cards two">
                        <button class="fd-card" class:on=move || any_token.get() on:click=move |_| any_token.set(true)><span class="fd-radio"></span><b>"Any token"</b>
                            <span class="small t2">"Every identity reads everything; refusals a real vault would give are marked in the log."</span></button>
                        <button class="fd-card" class:on=move || !any_token.get() on:click=move |_| any_token.set(false)><span class="fd-radio"></span><b>"Only identities with roles"</b>
                            <span class="small t2">{move || if cur.get().is_empty() { "Add them in Vault settings after.".to_string() } else { format!("Copies the identities of {}; change them in Vault settings after.", cur.get()) }}</span></button>
                    </div>
                    <div class="caps fd-l">"Soft delete"</div>
                    <div class="row" style="gap:10px"><ui::Seg options=vec![("7", "7 days"), ("30", "30 days"), ("90", "90 days")] value=Signal::derive(move || soft.get()) set=Callback::new(move |v| soft.set(v))/>
                        <span class="small cap">"deleted names stay taken; recreating gets 409"</span></div>
                    <div class="caps fd-l">"Comes with"</div>
                    <div class="small">{move || { let e: Vec<String> = all.get()["envSecrets"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
                        if e.is_empty() { "Nothing yet: this environment references no secret:// value.".to_string() } else {
                            format!("The {} value{} this environment already references ({}{})", e.len(), if e.len() == 1 { "" } else { "s" }, e.iter().take(3).map(|x| format!("secret://{x}")).collect::<Vec<_>>().join(", "), if e.len() > 3 { ", …" } else { "" }) } }}</div>
                    <div class="callout ok-fill" style="margin:16px 0 0;max-width:none">
                        <div class="small">"Set in the app"</div>
                        <div class="mono small ell">{move || format!("AZURE_KEYVAULT_URL={}", vault_url(&all.get(), &new_name.get()))}</div>
                        <div class="mono small ell">{move || format!("IDENTITY_ENDPOINT={}", s(&all.get(), "imds"))}</div>
                    </div>
                    <div class="fd-foot">
                        <span class="small cap grow">{move || { let n = new_name.get().trim().to_string(); if all.get()["vaults"].as_array().map(|a| a.iter().any(|v| s(v, "name") == n)).unwrap_or(false) { format!("{n} already exists") } else { "Trust the Stuntman Test CA where the app runs.".to_string() } }}</span>
                        <button class="btn" on:click=move |_| new_open.set(false)>"Cancel"</button><button class="btn primary" on:click=create>"Create vault"</button>
                    </div>
                </div>
            </Dialog>
        </div>
    }
}

/// The header of K1–K3 and K5: kind, vault, the four tabs, a search and the tab's action.
#[component]
pub fn Head(active: &'static str, #[prop(optional)] search: Option<RwSignal<String>>, #[prop(optional)] placeholder: &'static str, #[prop(into)] right: ViewFn) -> impl IntoView {
    let nav = use_navigate();
    let qm = use_query_map();
    let env = expect_context::<RwSignal<Value>>();
    let all = vaults();
    let cur = Memo::new(move |_| current(&all.get(), qm.get().get("vault")));
    let tabs: [(&'static str, &'static str); 4] = [("secrets", "Secrets"), ("keys", "Keys"), ("certificates", "Certificates"), ("log", "Access log")];
    view! {
        <div class="head">
            <div class="top">
                <div class="titled">
                    <div class="kind">{move || format!("Key Vault  ·  Azure Key Vault  ·  {}", s(&env.get(), "env").to_uppercase())}</div>
                    <h1>{move || { let c = cur.get(); if c.is_null() { "Key Vault".to_string() } else { s(&c, "name") } }}</h1>
                </div>
                <span class="grow"></span>
                {search.map(|q| view! { <input class="input" style="width:180px" placeholder=placeholder prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/> })}
                {right.run()}
            </div>
            <div class="tabs">
                {tabs.into_iter().map(|(key, label)| {
                    let nav = nav.clone();
                    view! {
                        <div class="tab" class:on=active == key on:click=move |_| nav(&href(key, &s(&cur.get_untracked(), "name"), ""), Default::default())>
                            {label}<span class="n">{move || match key { "secrets" => api::n(&cur.get(), "secrets").to_string(), "keys" => api::n(&cur.get(), "keys").to_string(), "certificates" => api::n(&cur.get(), "certificates").to_string(), _ => String::new() }}</span></div>
                    }
                }).collect_view()}
            </div>
        </div>
    }
}

/// No vault yet: what a vault is and how to make one.
pub fn no_vault() -> impl IntoView {
    view! {
        <div class="callout" style="margin:24px 32px">
            <div class="t">"No vault in this environment"</div>
            <div class="t2">"Create one with + New vault. Apps reach it at https://<vault>.vault.<environment domain>:8444 with the Azure SDK, and get a managed identity token at /metadata/identity/oauth2/token."</div>
        </div>
    }
}

/// The current vault name and item from the URL (falls back to the first vault).
pub fn sel() -> (Memo<String>, Memo<String>) {
    let qm = use_query_map();
    let all = vaults();
    // The vault in the URL wins, even before the list has it (right after creating it).
    (Memo::new(move |_| qm.get().get("vault").filter(|v| !v.is_empty()).unwrap_or_else(|| s(&current(&all.get(), None), "name"))), Memo::new(move |_| qm.get().get("item").unwrap_or_default()))
}

/// The "Who reads it · last hour" block shared by K1–K3.
pub fn readers_view(r: Vec<Value>, verb: &'static str) -> impl IntoView {
    if r.is_empty() {
        return view! { <div class="small cap" style="margin-top:8px">"Nobody in the last hour."</div> }.into_any();
    }
    r.into_iter().map(|x| {
        let cached = x["cached"] == json!(true);
        view! {
            <div style="margin-top:12px">
                <div class="row"><span class="mono grow">{let c = s(&x, "client"); if c.is_empty() { s(&x, "identity") } else { c }}</span><span class="mono small">{format!("{} {verb}", api::thousands(api::n(&x, "reads")))}</span></div>
                <div class="small cap">{format!("managed identity {}", s(&x, "identity"))}</div>
                <div class="small" class:ok=cached class:attn=!cached>{s(&x, "pattern")}</div>
            </div>
        }
    }).collect_view().into_any()
}

/// A provocation menu (Next reads / Next operations / Provoke): changes take effect at once.
#[component]
pub fn Provoke(kind: &'static str, #[prop(into)] name: Signal<String>, #[prop(into)] vault: Signal<String>, #[prop(into)] current: Signal<String>, options: Vec<(&'static str, &'static str)>) -> impl IntoView {
    let set = move |e: leptos::ev::Event| {
        let a = event_target_value(&e);
        let (n, v) = (name.get_untracked(), vault.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/keyvault/{kind}/{}/provoke", enc(&n)), json!({"answer": a, "vault": v, "by": "you"})).await {
                Ok(_) => ui::toast(if a == "normal" { "Back to normal".to_string() } else { format!("Next answers: {a}") }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <select class="select" style="margin-top:8px" on:change=set>
            {options.into_iter().map(|(v, l)| view! { <option value=v selected=move || { let c = current.get(); if c.is_empty() { v == "normal" } else { c == v } }>{l}</option> }).collect_view()}
        </select>
    }
}

/// Events (Event Grid) of one item.
pub fn events_view(ev: Vec<Value>, subscribers: Vec<Value>) -> impl IntoView {
    view! {
        {if ev.is_empty() { view! { <div class="small cap" style="margin-top:8px">"None sent yet."</div> }.into_any() } else {
            ev.into_iter().take(6).map(|e| view! { <div class="small" style="margin-top:8px">{format!("{}  ·  {}", s(&e, "event"), api::ago(api::n(&e, "at")))}</div> }).collect_view().into_any() }}
        <div class="small cap" style="margin-top:8px">{if subscribers.is_empty() { "No subscriber: set one in Settings to get Event Grid posts.".to_string() } else { format!("Sent to {}", subscribers.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")) }}</div>
    }
}
