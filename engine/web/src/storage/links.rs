//! S3 · Files › Links: Azure SAS and S3 presigned URLs, filters on the left, the link on the
//! right with what it allows, every use, Next use, Revoke and New link.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{who, FilesHead};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Pop},
};

fn valid_text(l: &Value) -> (String, &'static str) {
    match s(l, "state").as_str() {
        "revoked" => (format!("revoked by {}", who(&s(l, "revokedBy"))), "attn"),
        "expired" => (format!("expired {}", api::hm(api::n(l, "expiry"))), "danger"),
        _ => (format!("{} left", api::span(api::n(l, "expiry") - api::now() as i64)), ""),
    }
}

#[component]
pub fn Links() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let q = RwSignal::new(String::new());
    let data = RwSignal::new(Value::Null);
    let link = RwSignal::new(Value::Null);
    let state = RwSignal::new(String::new());
    let issued = RwSignal::new(String::new());
    let kind = RwSignal::new(String::new());
    let revoke = RwSignal::new(false);
    let new_open = RwSignal::new(false);
    let sel = Memo::new(move |_| qm.get().get("id").unwrap_or_default());
    let load = move || leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/storage/links").await {
            data.set(v);
        }
        let id = sel.get_untracked();
        if !id.is_empty() {
            link.set(api::get(&format!("/storage/links/{}", enc(&id))).await.unwrap_or(Value::Null));
        }
    });
    load();
    Effect::new(move |_| { sel.track(); load(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("storage.") { load(); }
        t
    });
    let shown = Memo::new(move |_| {
        let f = q.get().to_lowercase();
        data.get()["links"].as_array().cloned().unwrap_or_default().into_iter().filter(|l| {
            qm.get().get("account").map(|a| s(l, "account") == a).unwrap_or(true) && (state.get().is_empty() || s(l, "state") == state.get()) && (issued.get().is_empty() || s(l, "issuedBy") == issued.get()) && (kind.get().is_empty() || s(l, "kind") == kind.get())
                && (f.is_empty() || s(l, "path").to_lowercase().contains(&f) || s(l, "issuedBy").to_lowercase().contains(&f))
        }).collect::<Vec<_>>()
    });
    let filter_row = move |sig: RwSignal<String>, v: String, label: String, n: i64| {
        let v2 = v.clone();
        view! { <div class="crow" class:on=move || sig.get() == v on:click=move |_| sig.set(v2.clone())><span>{label}</span><span class="mono small cap">{n}</span></div> }
    };
    let act = move |path: String, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        let m = if path.ends_with("next-use") { "PUT" } else if path.ends_with("/remove") { "DELETE" } else { "POST" };
        let p = path.trim_end_matches("/remove").to_string();
        match api::send(m, &p, body).await { Ok(_) => ui::toast(done), Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="page">
            <FilesHead active="links" q=q placeholder="Path, issued by…" right=move || view! { <button class="btn" on:click=move |_| new_open.set(true)>"New link"</button> }/>
            <div class="body">
                <div class="list narrow tree">
                    {move || {
                        let c = data.get()["counts"].clone();
                        view! {
                            <div class="caps section" style="padding:18px 24px 8px">"State"</div>
                            {filter_row(state, String::new(), "All links".into(), api::n(&c, "all"))}
                            {filter_row(state, "valid".into(), "Valid".into(), api::n(&c, "valid"))}
                            {filter_row(state, "expired".into(), "Expired".into(), api::n(&c, "expired"))}
                            {filter_row(state, "revoked".into(), "Revoked".into(), api::n(&c, "revoked"))}
                            <div class="caps section" style="padding:18px 24px 8px">"Issued by"</div>
                            {c["issuedBy"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, n)| filter_row(issued, k.clone(), if k.len() > 26 { format!("{}…", &k[..24]) } else { k.clone() }, n.as_i64().unwrap_or(0))).collect_view()}
                            <div class="caps section" style="padding:18px 24px 8px">"Kind"</div>
                            {filter_row(kind, "sas".into(), "Azure SAS".into(), api::n(&c, "sas"))}
                            {filter_row(kind, "presigned".into(), "S3 presigned".into(), api::n(&c, "presigned"))}
                            <div class="small cap" style="padding:16px 24px">"A link is listed when someone makes one here or the first time a link the app signed is used."</div>
                        }
                    }}
                </div>
                <div class="pane files">
                    <div class="ftable">
                        <div class="fhead caps"><span class="grow">"Object"</span><span style="width:80px">"Allows"</span><span style="width:130px">"Valid"</span><span style="width:90px">"Uses"</span></div>
                        {move || {
                            let nav = nav.clone();
                            shown.get().into_iter().map(|l| {
                                let id = s(&l, "id");
                                let id2 = id.clone();
                                let (vt, cls) = valid_text(&l);
                                let refused = api::n(&l, "refused");
                                let nav = nav.clone();
                                view! { <div class="frow" class:on=move || sel.get() == id2 on:click=move |_| nav(&format!("/files/links?id={id}"), Default::default())>
                                    <div class="grow"><div class="mono">{s(&l, "path")}</div>
                                        <div class="small cap">{format!("{}  ·  {}{}  ·  {}", s(&l, "container"), if s(&l, "kind") == "presigned" { "S3 presigned  ·  " } else { "" }, who(&s(&l, "issuedBy")), api::hm(api::n(&l, "issuedAt")))}</div></div>
                                    <span class="mono small" style="width:80px">{s(&l, "allows").replace(',', ", ")}</span>
                                    <span class=format!("small {cls}") style="width:130px">{vt}</span>
                                    <span class="mono small" style="width:90px" class:danger={refused > 0}>{if refused > 0 { format!("{} refused", refused) } else { api::n(&l, "uses").to_string() }}</span></div> }
                            }).collect_view()
                        }}
                    </div>
                    {move || data.get()["links"].as_array().map(|x| x.is_empty()).unwrap_or(false).then(|| view! {
                        <div class="callout" style="margin:20px 24px"><div class="t">{format!("No links in {} yet", api::s(&env.get(), "env").to_uppercase())}</div>
                            <div class="t2">"When the app asks for a SAS or a presigned URL, it shows here with every use. You can also make one by hand to test a download page."</div>
                            <button class="btn" on:click=move |_| new_open.set(true)>"New link"</button></div> })}
                    <div class="small cap" style="padding:14px 24px">"Expired and revoked links stay 7 days, so a late use still shows up here."</div>
                </div>
                <div class="side envside">
                    {move || {
                        let l = link.get();
                        if l.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick a link to see what it allows and every use."</div> }.into_any(); }
                        let id = s(&l, "id");
                        let next = RwSignal::new(false);
                        let more = RwSignal::new(false);
                        let url = s(&l, "url");
                        let (u1, u2) = (url.clone(), url.clone());
                        let (i1, i2, i3) = (id.clone(), id.clone(), id.clone());
                        let (vt, cls) = valid_text(&l);
                        let obj_href = format!("/files?account={}&container={}&sel={}", enc(&s(&l, "account")), enc(&s(&l, "container")), enc(&s(&l, "path")));
                        view! {
                            <div class="row"><span class="h2" style="margin:0">"Read link"</span><span class="mono t2">{s(&l, "path").rsplit('/').next().unwrap_or("").to_string()}</span></div>
                            <div class="small cap">{format!("{}  ·  issued by {} at {}  ·  ", if s(&l, "kind") == "sas" { "Azure SAS" } else { "S3 presigned" }, who(&s(&l, "issuedBy")), api::hm(api::n(&l, "issuedAt")))}<span class=cls>{vt}</span></div>
                            <div class="urlbox mono">{url.clone()}</div>
                            <div class="row" style="gap:8px">
                                <button class="btn" on:click=move |_| ui::copy(&u1, "URL")>"Copy URL"</button>
                                <button class="btn" disabled=l["revokedAt"].is_string() || !l["revokedAt"].is_null() on:click=move |_| revoke.set(true)>"Revoke"</button>
                                <span class="anchor"><button class="btn icon" on:click=move |_| more.set(true)>"⋯"</button>
                                    <Pop open=more>
                                        <div class="mi" on:click={ let i1 = i1.clone(); move |_| act(format!("/storage/links/{}/extend", enc(&i1)), json!({"by": "1h"}), "Extended by 1 hour") }>"Extend by 1 hour"</div>
                                        <div class="mi" on:click={ let u2 = u2.clone(); move |_| ui::copy(&format!("curl -i '{u2}'"), "curl") }>"Copy as curl"</div>
                                        <a class="mi" href=obj_href.clone()>"Open the object"</a>
                                        <div class="rule"></div>
                                        <div class="mi danger" on:click={ let i2 = i2.clone(); move |_| act(format!("/storage/links/{}/remove", enc(&i2)), json!({}), "Removed from the list") }>"Remove from the list"</div>
                                    </Pop></span>
                            </div>
                            <div class="row" style="margin:18px 0 6px;gap:12px"><span style="width:76px">"Next use"</span>
                                <span class="anchor"><button class="btn" class:armed=!l["nextUse"].is_null() on:click=move |_| next.set(true)>{if l["nextUse"].is_null() { "Normal".to_string() } else { s(&l, "nextUse").replace('_', " ") }}<span class="small">"▾"</span></button>
                                    <Pop open=next>
                                        {[("normal", "Normal"), ("expired", "Expired now · 403 AuthenticationFailed"), ("bad_signature", "Wrong signature · 403"), ("ip_denied", "IP not allowed · 403"), ("permission", "Permission missing · 403"), ("not_yet_valid", "Not valid yet (clock skew)"), ("gone", "Object gone · 404")]
                                            .into_iter().map(|(v, lb)| { let i = i3.clone(); view! { <div class="mi" on:click=move |_| act(format!("/storage/links/{}/next-use", enc(&i)), json!({"result": v}), "Next use set")>{lb}</div> } }).collect_view()}
                                        <div class="rule"></div><div class="mh small cap">"Once, then back to normal"</div>
                                    </Pop></span>
                                <span class="small cap">"once, then normal"</span></div>
                            <div class="caps" style="margin:20px 0 8px">"What it allows"</div>
                            <table class="kv2">
                                <tr><td>"Permissions"</td><td class="mono">{s(&l, "allows").replace(',', ", ")}</td></tr>
                                <tr><td>"Valid"</td><td class="mono">{format!("{} → {}", if l["start"].is_null() { "—".into() } else { api::hm(api::n(&l, "start")) }, api::hm(api::n(&l, "expiry")))}</td></tr>
                                <tr><td>"Allowed IPs"</td><td class="mono">{if s(&l, "ips").is_empty() { "any".into() } else { s(&l, "ips") }}</td></tr>
                                <tr><td>"Protocol"</td><td class="mono">{if s(&l, "protocol").is_empty() { "any".into() } else { s(&l, "protocol") }}</td></tr>
                                <tr><td>"Signed with"</td><td class="mono">{if s(&l, "signedWith").is_empty() { "not checked (any key on)".into() } else { s(&l, "signedWith") }}</td></tr>
                            </table>
                            <div class="caps" style="margin:20px 0 8px">"Uses"</div>
                            {l["uses"].as_array().cloned().unwrap_or_default().into_iter().map(|u| {
                                let st = api::n(&u, "status");
                                let bad = st >= 400;
                                let what = if bad { format!("Refused{}", if s(&u, "provoked").is_empty() { String::new() } else { format!(": {}", s(&u, "provoked")) }) } else if s(&u, "method") == "HEAD" { "Checked".into() } else if s(&u, "method") == "PUT" { "Uploaded".into() } else { "Downloaded".into() };
                                view! { <div class="hrow"><span class="mono small cap">{api::hms(api::n(&u, "at"))}</span><div><div>{what}</div>
                                    <div class="small" class:danger=bad class:cap=!bad>{format!("{}  ·  {}  ·  from {}  ·  {}", s(&u, "method"), st, s(&u, "remote"), s(&u, "caller"))}</div></div></div> }
                            }).collect_view()}
                            {l["uses"].as_array().map(|x| x.is_empty()).unwrap_or(true).then(|| view! { <div class="small cap">"Not used yet."</div> })}
                            <a class="link" style="display:inline-block;margin-top:16px;font-size:13px" href=format!("/files?account={}&container={}&sel={}", enc(&s(&l, "account")), enc(&s(&l, "container")), enc(&s(&l, "path")))>"Open the object in Objects ›"</a>
                        }.into_any()
                    }}
                </div>
            </div>
            <Confirm open=revoke question=Signal::derive(|| "Revoke this link?".to_string())
                detail=Signal::derive(|| "Its next use answers 403 AuthenticationFailed, the same as Azure after the signing key rotates. The app is not told.".to_string())
                action="Revoke" on_yes=move |_| act(format!("/storage/links/{}/revoke", enc(&sel.get_untracked())), json!({}), "Revoked")/>
            <NewLink open=new_open/>
        </div>
    }
}

#[component]
fn NewLink(open: RwSignal<bool>) -> impl IntoView {
    let obj = RwSignal::new(String::new());
    let allows = RwSignal::new(vec!["read".to_string()]);
    let valid = RwSignal::new("1h".to_string());
    let ips = RwSignal::new(String::new());
    let made = RwSignal::new(String::new());
    let create = move |_| {
        let full = obj.get_untracked();
        let body = json!({"allows": allows.get_untracked(), "valid_for": valid.get_untracked(), "ips": ips.get_untracked()});
        leptos::task::spawn_local(async move {
            let accts = api::get("/storage/accounts").await.unwrap_or(Value::Null);
            let (c, p) = full.split_once('/').unwrap_or((&full, ""));
            let Some(a) = accts["accounts"].as_array().cloned().unwrap_or_default().into_iter().find(|a| a["containers"].as_array().map(|x| x.iter().any(|y| s(y, "name") == c)).unwrap_or(false)) else {
                ui::toast_err(format!("No container “{c}”: write container/path"));
                return;
            };
            match api::send("POST", &format!("/storage/{}/{}/links/{}", enc(&s(&a, "account")), enc(c), p.split('/').map(enc).collect::<Vec<_>>().join("/")), body).await {
                Ok(v) => { made.set(s(&v, "url")); ui::copy(&s(&v, "url"), "Link"); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <ui::Dialog open=open title="New link">
            <div class="small cap" style="margin:-10px 0 14px">"Signed by Stuntman; works only against Stuntman"</div>
            <div class="field"><label>"Object"</label><input class="input mono" placeholder="documents/2026/11/nda-acme.pdf" prop:value=move || obj.get() on:input=move |e| obj.set(event_target_value(&e))/></div>
            <div class="field" style="margin-top:12px"><label>"Allows"</label><div class="row" style="gap:16px">
                {["read", "write", "delete", "list"].into_iter().map(|k| view! {
                    <label class="check"><input type="checkbox" prop:checked=move || allows.get().iter().any(|x| x == k) on:change=move |_| allows.update(|v| if v.iter().any(|x| x == k) { v.retain(|x| x != k) } else { v.push(k.into()) })/>{k}</label>
                }).collect_view()}</div></div>
            <div class="row" style="margin-top:12px"><span style="width:100px">"Valid for"</span>
                <select class="select" on:change=move |e| valid.set(event_target_value(&e))>{[("10m", "10 minutes"), ("1h", "1 hour"), ("24h", "24 hours"), ("7d", "7 days")].into_iter().map(|(v, l)| view! { <option value=v selected=v == "1h">{l}</option> }).collect_view()}</select></div>
            <div class="row" style="margin-top:12px"><span style="width:100px">"Allowed IPs"</span><input class="input grow" placeholder="any" prop:value=move || ips.get() on:input=move |e| ips.set(event_target_value(&e))/></div>
            {move || (!made.get().is_empty()).then(|| view! { <div class="urlbox mono">{made.get()}</div><div class="small ok">"Copied. Recorded as made by you."</div> })}
            <div class="actions">
                <button class="btn" on:click=move |_| { open.set(false); made.set(String::new()); }>"Cancel"</button>
                <button class="btn primary" disabled=move || !obj.get().contains('/') on:click=create>"Create link"</button>
            </div>
        </ui::Dialog>
    }
}
