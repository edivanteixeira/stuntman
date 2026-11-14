//! I5 · Tenant settings: what the provider answers like, its issuer and discovery, signing keys
//! (current and next, rotation), lifetimes and clock skew, and the provider's own failures; the file.

use leptos::prelude::*;
use serde_json::{json, Value};

use super::{fetch, sel, Head};
use crate::{
    api::{self, enc, s},
    ui,
    whatsapp::YamlMarked,
};

#[component]
pub fn Tenant() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let p = sel();
    let data = fetch("tenant", p);
    let draft = RwSignal::new(Value::Null);
    let before = RwSignal::new(String::new());
    let shown_yaml = RwSignal::new(String::new());
    let elsewhere = RwSignal::new(false);
    let editable = |t: &Value| json!({"flavour": t["flavour"], "lifetimes": t["lifetimes"], "otherTenants": t["otherTenants"]});
    let last = RwSignal::new(Value::Null);
    let rm_open = RwSignal::new(false);
    let nav = leptos_router::hooks::use_navigate();
    let remove = Callback::new(move |_: ()| {
        let name = p.get_untracked();
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/identity/providers/{}", enc(&name)), json!({})).await {
                Ok(_) => { ui::toast(format!("{name} removed")); nav("/identity", Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    });
    Effect::new(move |_| {
        let t = data.get();
        if t.is_null() { return; }
        let e = editable(&t);
        let y = s(&t, "yaml");
        let old = shown_yaml.get_untracked();
        if !old.is_empty() && old != y { before.set(old); }
        shown_yaml.set(y);
        // Edits here and the file changed elsewhere: say so instead of overwriting them.
        let (d, prev) = (draft.get_untracked(), last.get_untracked());
        if !d.is_null() && d != prev && e != prev { elsewhere.set(true); } else if d.is_null() || d == prev { draft.set(e.clone()); }
        last.set(e);
    });
    let saved = Memo::new(move |_| editable(&data.get()));
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != saved.get());
    let save = move |_| {
        let d = draft.get_untracked();
        let prov = p.get_untracked();
        leptos::task::spawn_local(async move {
            let b = json!({"flavour": d["flavour"], "id_lifetime": d["lifetimes"]["id"], "access_lifetime": d["lifetimes"]["access"], "refresh_lifetime": d["lifetimes"]["refresh"], "clock_skew": d["lifetimes"]["clock_skew"], "other_tenants": d["otherTenants"]});
            match api::send("PUT", &format!("/identity/{}/tenant", enc(&prov)), b).await { Ok(_) => { ui::toast("Saved in the provider file"); elsewhere.set(false); last.set(d.clone()); } Err(e) => ui::toast_err(e) }
        });
    };
    let rotate = move |_| { let prov = p.get_untracked(); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/identity/{}/keys/rotate", enc(&prov)), json!({})).await { Ok(x) => ui::toast(format!("Signing with kid {} now; {} published as next", s(&x, "current"), s(&x, "next"))), Err(e) => ui::toast_err(e) } }); };
    let provoke = move |part: &'static str, ans: String| { let prov = p.get_untracked(); leptos::task::spawn_local(async move {
        match api::send("POST", &format!("/identity/{}/provoke-idp", enc(&prov)), json!({"part": part, "answer": ans})).await { Ok(_) => ui::toast(format!("{part}: {ans}")), Err(e) => ui::toast_err(e) } }); };
    let _ = live;
    let life = move |key: &'static str, opts: Vec<(&'static str, &'static str)>| view! {
        <select class="select" on:change=move |e| draft.update(|d| d["lifetimes"][key] = json!(event_target_value(&e)))>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || s(&draft.get()["lifetimes"], key) == v>{l}</option> }).collect_view()}</select>
    };
    let prov_sel = move |part: &'static str, opts: Vec<(&'static str, &'static str)>| view! {
        <select class="select" on:change=move |e| provoke(part, event_target_value(&e))>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || s(&data.get()["provoke"], part) == v>{l}</option> }).collect_view()}</select>
    };
    view! {
        <div class="page">
            <Head active="tenant" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"1 unsaved change"</span><button class="btn" on:click=move |_| draft.set(saved.get_untracked())>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button>
            }/>
            <div class="body">
                {move || {
                    let t = data.get();
                    if t.is_null() { return view! { <super::NoProvider/> }.into_any(); }
                    let d = draft.get();
                    let iss = s(&t, "issuer"); let i2 = iss.clone();
                    let skew = s(&d["lifetimes"], "clock_skew");
                    let since = t["keys"]["currentSince"].as_i64().unwrap_or(0);
                    let rotated = t["keys"]["rotatedAt"].as_i64().unwrap_or(0);
                    let fresh = rotated > 0 && api::now() as i64 - rotated < 3_600_000;
                    let pv = &t["provoke"];
                    view! {
                        <div class="pane pad form"><div style="max-width:720px">
                            {move || elsewhere.get().then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">"The provider file changed while you were editing"</div>
                                <div class="t2" style="margin:0">"Changed through the API, MCP or the file. Load it, or keep yours and save over it."</div>
                                <div class="row" style="gap:16px;margin-top:8px"><button class="link" on:click=move |_| { elsewhere.set(false); draft.set(saved.get_untracked()); }>"Load the new file"</button><button class="link" on:click=save>"Keep mine and save over it"</button></div></div> })}
                            {fresh.then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("Signing with kid {} since {}", s(&t["keys"], "current"), api::hm(rotated))}</div>
                                <div class="t2" style="margin:0">"Apps that cached the JWKS at start-up reject new tokens until they refresh it."</div></div> })}
                            {(s(pv, "jwks") == "unavailable").then(|| view! { <div class="callout danger-l" style="max-width:none;margin:0 0 14px"><div class="t">"/discovery/v2.0/keys answers 503"</div><div class="t2" style="margin:0">"Apps with a cached key keep working; apps without one fail every token validation."</div></div> })}
                            {(s(pv, "discovery") == "wrong_issuer").then(|| view! { <div class="callout danger-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("Discovery says the issuer is …/{}/v2.0", t["otherTenants"][0].as_str().unwrap_or("fabrikam-stg"))}</div><div class="t2" style="margin:0">"The app must reject the tokens even though the signature is valid."</div></div> })}
                            {(!skew.is_empty() && skew != "0s").then(|| view! { <div class="callout attn-l" style="max-width:none;margin:0 0 14px"><div class="t">{format!("Tokens issued {skew} in the future")}</div><div class="t2" style="margin:0">"nbf and iat ahead of the app's clock; most libraries allow 5 min, so more than that fails."</div></div> })}
                            <div class="caps">"Provider"</div>
                            <table class="kv2" style="width:100%;margin-top:8px">
                                <tr><td style="width:150px">"Answers like"</td><td><select class="select" on:change=move |e| draft.update(|d| d["flavour"] = json!(event_target_value(&e)))>
                                    {t["flavours"].as_array().cloned().unwrap_or_default().into_iter().map(|f| { let id = s(&f, "id"); let sel = s(&d, "flavour") == id; view! { <option value=id selected=sel>{s(&f, "label")}</option> } }).collect_view()}</select>
                                    <span class="small cap" style="margin-left:12px">"users and apps stay"</span></td></tr>
                                <tr><td>"Issuer"</td><td class="mono small">{iss}<button class="link" style="margin-left:12px" on:click=move |_| ui::copy(&i2, "Issuer")>"Copy"</button></td></tr>
                                <tr><td>"Discovery"</td><td class="mono small" style="word-break:break-all">{s(&t, "discovery")}</td></tr>
                                <tr><td>"Other tenants"</td><td><input class="input mono" style="width:260px" prop:value=d["otherTenants"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
                                    on:change=move |e| draft.update(|d| d["otherTenants"] = json!(event_target_value(&e).split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>()))/>
                                    <span class="small cap" style="margin-left:12px">"to test \"wrong tenant\""</span></td></tr>
                            </table>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="caps">"Signing keys (JWKS)"</div>
                            <table class="kv2" style="width:100%;margin-top:8px">
                                <tr><td style="width:150px">"Current"</td><td class="mono">{format!("kid {}  ·  RS256{}", s(&t["keys"], "current"), if since > 0 { format!("  ·  since {}", crate::environments::day(since)) } else { String::new() })}</td></tr>
                                <tr><td>"Next"</td><td class="mono">{format!("kid {}  ·  published, not signing yet", s(&t["keys"], "next"))}</td></tr>
                            </table>
                            <div class="row" style="gap:16px;margin-top:8px"><button class="link" on:click=rotate>"Rotate signing key now"</button><span class="small cap">"new tokens use the next key; apps that cache JWKS forever break on the following rotation"</span></div>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="caps">"Lifetimes"</div>
                            <table class="kv2" style="width:100%;margin-top:8px">
                                <tr><td style="width:150px">"ID and access"</td><td>{life("id", vec![("5m", "5 min"), ("15m", "15 min"), ("60m", "60 min"), ("24h", "24 h")])}{life("access", vec![("5m", "5 min"), ("15m", "15 min"), ("60m", "60 min"), ("24h", "24 h")])}</td></tr>
                                <tr><td>"Refresh"</td><td>{life("refresh", vec![("1h", "1 h"), ("24h", "24 h"), ("7d", "7 days"), ("90d", "90 days")])}</td></tr>
                                <tr><td>"Clock skew"</td><td>{life("clock_skew", vec![("0s", "0 s"), ("2m", "+2 min"), ("10m", "+10 min")])}<span class="small cap" style="margin-left:12px">"+10 min issues tokens \"from the future\""</span></td></tr>
                            </table>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="caps">"Provoke"</div>
                            <table class="kv2" style="width:100%;margin-top:8px">
                                <tr><td style="width:150px">"Discovery"</td><td>{prov_sel("discovery", vec![("normal", "Normal"), ("500", "500"), ("slow", "Slow 10 s"), ("wrong_issuer", "Wrong issuer")])}<span class="small cap" style="margin-left:12px">"500, slow 10 s, wrong issuer"</span></td></tr>
                                <tr><td>"JWKS"</td><td>{prov_sel("jwks", vec![("normal", "Normal"), ("unavailable", "Unavailable"), ("kid_missing", "Kid missing"), ("wrong_key", "Wrong key")])}<span class="small cap" style="margin-left:12px">"unavailable, kid missing, wrong key"</span></td></tr>
                                <tr><td>"Token endpoint"</td><td>{prov_sel("token", vec![("normal", "Normal"), ("invalid_grant", "invalid_grant"), ("500", "500"), ("slow", "Slow 10 s")])}<span class="small cap" style="margin-left:12px">"invalid_grant, 500, slow"</span></td></tr>
                            </table>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="small cap">{format!("Fields come from {}. Users and apps are in the same file.", s(&t, "file"))}</div>
                            <div style="border-top:1px solid var(--rule);margin:18px 0"></div>
                            <div class="row" style="gap:12px"><button class="btn danger" on:click=move |_| rm_open.set(true)>"Remove provider"</button>
                                <span class="small cap">"Deletes its users, apps, tenant, sign-ins and keys. Apps pointed at its Authority get 404."</span></div>
                            <ui::Confirm open=rm_open question=Signal::derive(move || format!("Remove {}?", p.get())) detail=Signal::derive(|| "Its file and its sign-ins are deleted. This cannot be undone.".to_string()) action="Remove" on_yes=remove/>
                        </div></div>
                        <div class="side yaml" style="width:420px">
                            <div class="row"><span class="caps">{s(&t, "file")}</span><span class="small ok" style="margin-left:10px">"live"</span></div>
                            <YamlMarked text=Signal::derive(move || shown_yaml.get()) before=Signal::derive(move || before.get())/>
                        </div>
                    }.into_any()
                }}
            </div>
        </div>
    }
}

