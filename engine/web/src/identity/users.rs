//! I3 · Users: synthetic people and the claims that go into their tokens; consent, MFA, enabled,
//! groups (over 200: the overage path), roles, extra claims; the id token they'd get, as a preview.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Map, Value};

use super::{fetch, href, sel, Head};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Dialog, Toggle},
};

fn list(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// The id token this user would get from an app now (Entra flavour), as the engine builds it.
fn preview(authority: &str, tid: &str, u: &Value, app: &str) -> Value {
    let mut c = Map::new();
    c.insert("iss".into(), json!(authority));
    c.insert("aud".into(), json!(app));
    c.insert("oid".into(), u["oid"].clone());
    c.insert("email".into(), u["email"].clone());
    c.insert("name".into(), u["name"].clone());
    let g = list(&u["groups"]);
    if g.len() > 200 { c.insert("_claim_names".into(), json!({"groups": "src1"})); } else if !g.is_empty() { c.insert("groups".into(), json!(g)); }
    if !list(&u["roles"]).is_empty() { c.insert("roles".into(), u["roles"].clone()); }
    c.insert("tid".into(), json!(tid));
    c.insert("amr".into(), json!(if u["mfa"] == json!(true) { vec!["pwd", "mfa"] } else { vec!["pwd"] }));
    if let Some(e) = u["extra"].as_object() { for (k, v) in e { c.insert(k.clone(), v.clone()); } }
    Value::Object(c)
}

/// Chips of a list field with remove and add.
#[component]
fn Chips(draft: RwSignal<Value>, key: &'static str, placeholder: &'static str) -> impl IntoView {
    let new = RwSignal::new(String::new());
    let add = move || { let v = new.get_untracked().trim().to_string(); if v.is_empty() { return; } draft.update(|d| { let mut l = list(&d[key]); if !l.contains(&v) { l.push(v); } d[key] = json!(l); }); new.set(String::new()); };
    view! {
        <div class="row" style="gap:6px;flex-wrap:wrap">
            {move || { let all = list(&draft.get()[key]); let more = all.len().saturating_sub(12); view! {
                {all.into_iter().take(12).map(|g| { let g2 = g.clone(); view! {
                    <button class="chip" on:click=move |_| draft.update(|d| { let mut l = list(&d[key]); l.retain(|x| *x != g2); d[key] = json!(l); })>{format!("{g}  ×")}</button> } }).collect_view()}
                {(more > 0).then(|| view! { <span class="small cap">{format!("+{more} more")}</span> })}
            } }}
            <input class="input" style="width:120px;height:26px" placeholder=placeholder prop:value=move || new.get() on:input=move |e| new.set(event_target_value(&e))
                on:keydown=move |e: web_sys::KeyboardEvent| if e.key() == "Enter" { add() }/>
        </div>
    }
}

#[component]
pub fn Users() -> impl IntoView {
    let nav = use_navigate();
    let qm = use_query_map();
    let p = sel();
    let data = fetch("users", p);
    let apps = fetch("apps", p);
    let tenant = fetch("tenant", p);
    let item = Memo::new(move |_| qm.get().get("user").unwrap_or_default());
    let cur = Memo::new(move |_| { let us = data.get()["users"].as_array().cloned().unwrap_or_default(); let i = item.get(); us.iter().find(|u| s(u, "id") == i).cloned().or_else(|| us.first().cloned()).unwrap_or(Value::Null) });
    let draft = RwSignal::new(Value::Null);
    Effect::new(move |_| draft.set(cur.get()));
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != cur.get());
    let set = move |k: &'static str, v: Value| draft.update(|d| d[k] = v);
    let save = move |_| {
        let d = draft.get_untracked();
        let prov = p.get_untracked();
        leptos::task::spawn_local(async move {
            let b = json!({"name": d["name"], "email": d["email"], "tenant": d["tenant"], "guest": d["guest"], "groups": d["groups"], "roles": d["roles"], "mfa": d["mfa"], "phone_lost": d["phone_lost"], "enabled": d["enabled"], "extra": d["extra"]});
            match api::send("PUT", &format!("/identity/{}/users/{}", enc(&prov), enc(&s(&d, "id"))), b).await { Ok(_) => ui::toast("Saved: the next tokens carry these claims"), Err(e) => ui::toast_err(e) }
        });
    };
    let revoke = move |app: String| { let (prov, u) = (p.get_untracked(), s(&cur.get_untracked(), "id")); leptos::task::spawn_local(async move {
        match api::send("DELETE", &format!("/identity/{}/users/{}/consents/{}", enc(&prov), enc(&u), enc(&app)), Value::Null).await { Ok(x) => ui::toast(format!("Consent revoked; {} refresh token(s) stopped", api::n(&x, "refreshTokensStopped"))), Err(e) => ui::toast_err(e) } }); };
    let (x_key, x_val) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let add_open = RwSignal::new(false);
    let (n_name, n_email, n_tenant) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let create = move |_| {
        let (name, email, t, prov) = (n_name.get_untracked().trim().to_string(), n_email.get_untracked().trim().to_string(), n_tenant.get_untracked(), p.get_untracked());
        let id = email.split('@').next().unwrap_or("").to_lowercase();
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            let mut b = json!({"name": name, "email": email});
            if t == "guest" { b["guest"] = json!(true); } else if !t.is_empty() { b["tenant"] = json!(t); }
            match api::send("PUT", &format!("/identity/{}/users/{}", enc(&prov), enc(&id)), b).await {
                Ok(x) => { add_open.set(false); ui::toast(format!("{} added", s(&x["user"], "name"))); nav(&href("users", &prov, &format!("&user={}", enc(&s(&x["user"], "id")))), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let del = RwSignal::new(false);
    let do_del = move |_| { let (prov, u) = (p.get_untracked(), s(&cur.get_untracked(), "id")); let nav = use_navigate(); leptos::task::spawn_local(async move {
        match api::send("DELETE", &format!("/identity/{}/users/{}", enc(&prov), enc(&u)), Value::Null).await { Ok(_) => { ui::toast(format!("{u} removed")); nav(&href("users", &prov, ""), Default::default()); } Err(e) => ui::toast_err(e) } }); };
    let first_app = Memo::new(move |_| apps.get()["apps"].as_array().and_then(|a| a.iter().find(|x| s(x, "type") == "web" || s(x, "type") == "spa").map(|x| s(x, "client_id"))).unwrap_or("acme-app".into()));
    view! {
        <div class="page">
            <Head active="users" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"1 unsaved change"</span><button class="btn" on:click=move |_| draft.set(cur.get_untracked())>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button>
            }/>
            <div class="body">
                <div class="list narrow kvcol">
                    <div class="caps" style="padding:16px 20px 6px">"Users"</div>
                    {move || data.get()["users"].as_array().cloned().unwrap_or_default().into_iter().map(|u| {
                        let id = s(&u, "id"); let (i2, i3) = (id.clone(), id.clone()); let nav = nav.clone();
                        let mut sub: Vec<String> = vec![];
                        if u["mfa"] == json!(true) { sub.push("MFA".into()); }
                        if u["enabled"] == json!(false) { sub.push("disabled".into()); }
                        if u["guest"] == json!(true) { sub.push("guest".into()); }
                        if u["otherTenant"] == json!(true) { sub.push(format!("tenant {}", s(&u, "tenant"))); }
                        if u["phone_lost"] == json!(true) { sub.push("phone lost".into()); }
                        let g = list(&u["groups"]); if g.len() > 3 { sub.push(format!("{} groups", g.len())); } else if !g.is_empty() { sub.push(g.join(", ")); }
                        if u["consented"].as_array().map(|a| a.is_empty()).unwrap_or(true) && u["enabled"] != json!(false) && u["otherTenant"] != json!(true) { sub.push("no consent yet".into()); }
                        view! { <div class="kvrow" class:on=move || s(&cur.get(), "id") == i2 on:click=move |_| nav(&href("users", &p.get_untracked(), &format!("&user={}", enc(&i3))), Default::default())>
                            <div>{s(&u, "name")}</div><div class="small cap ell">{sub.join("  ·  ")}</div></div> }
                    }).collect_view()}
                    <div style="padding:10px 20px"><button class="link" on:click=move |_| { n_name.set(String::new()); n_email.set(String::new()); n_tenant.set(String::new()); add_open.set(true); }>"+ Add user"</button></div>
                    <div class="small cap" style="padding:4px 20px">"Users are synthetic people; their claims go into every token."</div>
                </div>
                {move || {
                    let d = draft.get();
                    if d.is_null() { return view! { <div class="pane pad"><div class="callout"><div class="t">"No users yet"</div><div class="t2">"Add one: name, email, tenant. The object id is generated and stays fixed. Synthetic names only; real-looking domains are refused."</div></div></div> }.into_any(); }
                    let t = data.get();
                    let tenants: Vec<String> = list(&t["tenants"]);
                    let home = s(&t, "tenant");
                    let groups = list(&d["groups"]);
                    let consented = list(&d["consented"]);
                    let uname = s(&d, "name");
                    let uid = s(&d, "id");
                    view! {
                        <div class="pane pad form"><div style="max-width:620px">
                            {(groups.len() > 200).then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("{uname} is in {} groups", groups.len())}</div>
                                <div class="t2" style="margin:0">"Over 200 the provider leaves groups out of the token and sends a \"groups overage\" claim; the app must call the Graph API instead. Stuntman does the same."</div></div> })}
                            {(d["enabled"] == json!(false)).then(|| view! { <div class="callout danger-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("{uname}  ·  account disabled")}</div>
                                <div class="t2" style="margin:0">"Sign-ins stop at the login page (AADSTS50057); refresh tokens are refused at the next use."</div></div> })}
                            {(d["guest"] == json!(true)).then(|| view! { <div class="callout" style="max-width:none;margin:0 0 14px"><div class="t">{format!("{uname}  ·  guest from a personal account")}</div>
                                <div class="t2" style="margin:0">"idp claim set, upn with #EXT#, as guests look in the real tenant."</div></div> })}
                            <table class="kv2" style="width:100%">
                                <tr><td style="width:120px">"Display name"</td><td><input class="input" style="width:300px" prop:value=s(&d, "name") on:input=move |e| set("name", json!(event_target_value(&e)))/></td></tr>
                                <tr><td>"Email"</td><td><input class="input mono" style="width:100%" prop:value=s(&d, "email") on:input=move |e| set("email", json!(event_target_value(&e)))/></td></tr>
                                <tr><td>"Object id"</td><td><div class="copybox mono" style="width:100%">{s(&d, "oid")}</div><div class="small cap">"stable: the app keys users by it (oid / sub)"</div></td></tr>
                                <tr><td>"Tenant"</td><td><select class="select" on:change=move |e| { let v = event_target_value(&e); if v == "guest" { set("guest", json!(true)); set("tenant", json!(home.clone())); } else { set("guest", json!(false)); set("tenant", json!(v)); } }>
                                    {tenants.iter().map(|x| { let x2 = x.clone(); let lbl = if *x == s(&t, "tenant") { format!("{x} (home)") } else { x.clone() }; let sel = d["guest"] != json!(true) && s(&d, "tenant") == *x; view! { <option value=x2 selected=sel>{lbl}</option> } }).collect_view()}
                                    <option value="guest" selected=d["guest"] == json!(true)>{format!("Guest in {}", s(&t, "tenant"))}</option></select></td></tr>
                                <tr><td>"Groups"</td><td><Chips draft=draft key="groups" placeholder="+ group"/></td></tr>
                                <tr><td>"App roles"</td><td><Chips draft=draft key="roles" placeholder="+ role"/></td></tr>
                            </table>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="row" style="gap:12px"><Toggle on=Signal::derive(move || draft.get()["enabled"] == json!(true)) set=Callback::new(move |b: bool| set("enabled", json!(b)))/><span>"Account enabled"</span></div>
                            <div class="row" style="gap:12px;margin-top:10px"><Toggle on=Signal::derive(move || draft.get()["mfa"] == json!(true)) set=Callback::new(move |b: bool| set("mfa", json!(b)))/><span>"MFA required"</span><span class="small cap" style="margin-left:24px">"code: any 6 digits; 000000 fails"</span></div>
                            <div class="row" style="gap:12px;margin-top:10px"><Toggle on=Signal::derive(move || draft.get()["phone_lost"] == json!(true)) set=Callback::new(move |b: bool| set("phone_lost", json!(b)))/><span>"Phone lost"</span><span class="small cap" style="margin-left:24px">"every MFA code fails"</span></div>
                            <div class="row" style="gap:16px;margin-top:16px;align-items:flex-start"><span style="width:120px">"Consented to"</span><div>
                                {if consented.is_empty() { view! { <span class="small cap">"Nothing yet: the next sign-in shows the consent page."</span> }.into_any() } else {
                                    consented.into_iter().map(|c| { let c2 = c.clone(); view! { <div>{c}<button class="link" style="margin-left:12px" on:click=move |_| revoke(c2.clone())>"Revoke consent"</button></div> } }).collect_view().into_any() }}
                            </div></div>
                            <div class="caps" style="margin-top:22px">"Extra claims"</div>
                            <table class="kv">{d["extra"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| { let k2 = k.clone(); view! {
                                <tr><td>{k}</td><td class="mono">{v.as_str().map(String::from).unwrap_or(v.to_string())}</td><td style="width:20px"><button class="link plain" on:click=move |_| draft.update(|d| { if let Some(o) = d["extra"].as_object_mut() { o.remove(&k2); } })>"×"</button></td></tr> } }).collect_view()}</table>
                            <div class="row" style="gap:8px;margin-top:8px">
                                <input class="input mono" style="width:150px" placeholder="employeeId" prop:value=move || x_key.get() on:input=move |e| x_key.set(event_target_value(&e))/>
                                <input class="input mono" style="width:180px" placeholder="E-10442" prop:value=move || x_val.get() on:input=move |e| x_val.set(event_target_value(&e))/>
                                <button class="link" on:click=move |_| { let (k, v) = (x_key.get_untracked().trim().to_string(), x_val.get_untracked()); if !k.is_empty() { draft.update(|d| { if !d["extra"].is_object() { d["extra"] = json!({}); } d["extra"][k] = json!(v); }); x_key.set(String::new()); x_val.set(String::new()); } }>"+ Add claim"</button>
                            </div>
                            <div style="margin-top:22px"><button class="link danger" on:click=move |_| del.set(true)>"Remove user"</button></div>
                        </div></div>
                        <div class="side">
                            <div class="caps">{move || format!("ID token for {}  ·  preview", first_app.get())}</div>
                            <pre class="yaml" style="white-space:pre-wrap;word-break:break-all;margin-top:8px">{move || serde_json::to_string_pretty(&preview(&s(&tenant.get(), "issuer"), &s(&tenant.get(), "tenantId"), &draft.get(), &first_app.get())).unwrap_or_default()}</pre>
                            <div class="small cap">"Changing a field changes the next tokens; tokens already issued keep their claims until they expire."</div>
                            <div style="margin-top:12px"><a class="link" href=href("signins", &p.get_untracked(), &format!("&user={}", enc(&uid)))>{format!("Sign-ins of {uname} ›")}</a></div>
                        </div>
                    }.into_any()
                }}
            </div>
            <Dialog open=add_open title="Add user">
                <div class="fd narrow">
                    <div class="small t2">"A synthetic person. The object id is generated and stays fixed; real-looking domains are refused."</div>
                    <div class="caps fd-l">"Name"</div><input class="input" style="width:100%" placeholder="Grace Hall" prop:value=move || n_name.get() on:input=move |e| n_name.set(event_target_value(&e))/>
                    <div class="caps fd-l">"Email"</div><input class="input mono" style="width:100%" placeholder="grace.hall@contoso.example.com" prop:value=move || n_email.get() on:input=move |e| n_email.set(event_target_value(&e))/>
                    <div class="caps fd-l">"Tenant"</div>
                    <select class="select" on:change=move |e| n_tenant.set(event_target_value(&e))>
                        {move || { let t = data.get(); let home = s(&t, "tenant"); list(&t["tenants"]).into_iter().map(|x| { let lbl = if x == home { format!("{x} (home)") } else { x.clone() }; view! { <option value=x>{lbl}</option> } }).collect_view() }}
                        <option value="guest">"Guest (personal account)"</option></select>
                    <div class="fd-foot"><span class="grow"></span><button class="btn" on:click=move |_| add_open.set(false)>"Cancel"</button><button class="btn primary" on:click=create>"Add user"</button></div>
                </div>
            </Dialog>
            <Confirm open=del question=Signal::derive(move || format!("Remove {}?", s(&cur.get(), "name"))) detail="Their sign-ins stop and their refresh tokens are refused. The sign-in history stays." action="Remove" on_yes=do_del/>
        </div>
    }
}
