//! W4 · Number settings: the number block of the environment (whatsapp/number.yaml): ids, access,
//! the webhook to the app (verification, signature, a forged one on purpose), tier and quality.

use leptos::prelude::*;
use serde_json::{json, Value};

use super::{pretty, Head};
use crate::{
    api::{self, s},
    ui::{self, Toggle},
};

#[component]
pub fn Number() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let data = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let bad_kind = RwSignal::new("wrong_signature".to_string());
    let bad_res = RwSignal::new(Value::Null);
    let elsewhere = RwSignal::new(false);
    let yaml_before = RwSignal::new(String::new());
    let load = move |keep: bool| leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/whatsapp/number").await {
            let dirty = draft.get_untracked() != data.get_untracked()["number"];
            if keep && dirty { if v["number"] != data.get_untracked()["number"] { elsewhere.set(true); } return; }
            draft.set(v["number"].clone());
            let old = s(&data.get_untracked(), "yaml");
            if !old.is_empty() && old != s(&v, "yaml") { yaml_before.set(old); }
            data.set(v);
        }
    });
    load(false);
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic") == "whatsapp.number" { load(true); } t });
    let dirty = Memo::new(move |_| !draft.get().is_null() && draft.get() != data.get()["number"]);
    let save = move |_| {
        let d = draft.get_untracked();
        leptos::task::spawn_local(async move {
            let body = json!({"any_token": d["access"]["any_token"], "tier": d["limits"]["tier"], "throughput": d["limits"]["throughput"], "quality": d["quality"],
                "webhook_url": d["webhook"]["url"], "sign": d["webhook"]["sign"], "display_name": d["display"]["name"], "approve_after": d["approve_after"], "api_versions": d["api_versions"]});
            match api::send("PUT", "/whatsapp/number", body).await {
                // Our own save: mark it saved at once, so its live echo is not taken for a change elsewhere.
                Ok(v) => { data.update(|x| x["number"] = v["number"].clone()); draft.set(v["number"].clone()); ui::toast("Saved in whatsapp/number.yaml"); elsewhere.set(false); load(false); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let act = move |path: &'static str, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        match api::send("POST", path, body).await {
            Ok(v) => { if path.ends_with("/bad") { bad_res.set(v); } else { ui::toast(done); } elsewhere.set(false); load(false); }
            Err(e) => ui::toast_err(e),
        }
    });
    let sel = move |path: &'static str, opts: Vec<(&'static str, &'static str)>| view! {
        <select class="input" style="width:200px" on:change=move |e| { let v = event_target_value(&e); draft.update(|d| { if let Some(p) = d.pointer_mut(path) { *p = if path.ends_with("throughput") { json!(v.parse::<u64>().unwrap_or(80)) } else { json!(v) }; } }) }>
            {opts.into_iter().map(|(v, l)| view! { <option value=v selected=move || draft.get().pointer(path).map(|x| x.as_str().map(String::from).unwrap_or(x.to_string())).unwrap_or_default() == v>{l}</option> }).collect_view()}
        </select>
    };
    view! {
        <div class="page">
            <Head active="number" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"Unsaved change"</span><button class="btn" on:click=move |_| draft.set(data.get_untracked()["number"].clone())>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=save>"Save"</button>
            }/>
            {move || elsewhere.get().then(|| view! { <div class="strip">"whatsapp/number.yaml changed while you were editing."
                <button class="link" on:click=move |_| { elsewhere.set(false); draft.set(Value::Null); load(false); }>"Load the new file"</button>
                <button class="link" on:click=save>"Keep mine and save over it"</button></div> })}
            <div class="body">
                <div class="pane pad form"><div style="max-width:720px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let v = data.get();
                        let ver = v["verification"].clone();
                        let pnid = s(&d, "phone_number_id");
                        let waba = s(&d, "waba_id");
                        let expired = !d["tokens_expired_at"].is_null();
                        view! {
                            <div class="caps">"Number"</div>
                            <table class="kv2" style="margin:8px 0 18px">
                                <tr><td>"Phone number id"</td><td class="mono">{pnid.clone()}</td><td><button class="link" on:click=move |_| ui::copy(&pnid, "id")>"Copy"</button></td></tr>
                                <tr><td>"Display number"</td><td class="mono">{pretty(&s(&d["display"], "number"))}</td><td class="small cap">{format!("shown as “{}”", s(&d["display"], "name"))}</td></tr>
                                <tr><td>"Business account"</td><td class="mono">{waba.clone()}</td><td><button class="link" on:click=move |_| ui::copy(&waba, "id")>"Copy"</button></td></tr>
                                <tr><td>"Graph base URL"</td><td class="mono">{s(&v, "graphBase")}</td><td></td></tr>
                                <tr><td>"API versions"</td><td>{sel("/api_versions", vec![("any", "any"), ("v18.0-v21.0", "v18.0 to v21.0"), ("v20.0-v25.0", "v20.0 to v25.0"), ("v25.0-v25.0", "v25.0 only")])}</td>
                                    <td class="small cap">"other versions get 2500 “Unknown path components”"</td></tr>
                            </table>
                            <div class="caps">"Access"</div>
                            <div class="row" style="gap:10px;margin:8px 0"><Toggle on=Signal::derive(move || draft.get()["access"]["any_token"] == true) set=move |b: bool| draft.update(|x| x["access"]["any_token"] = json!(b))/>
                                <span>"Accept any bearer token"</span><span class="small cap">"off: only the token in Secrets › whatsapp-token"</span></div>
                            {if expired {
                                view! { <div class="callout danger-fill small">{format!("Tokens expired at {}: every call gets 190 “Session has expired”.", api::hm(api::n(&d, "tokens_expired_at")))}
                                    <button class="link" on:click=move |_| act("/whatsapp/number/tokens/restore", json!({}), "Tokens work again")>"Restore tokens"</button></div> }.into_any()
                            } else {
                                view! { <div class="row small"><button class="link danger" on:click=move |_| act("/whatsapp/number/tokens/expire", json!({}), "Tokens expired: the next call gets 190")>"Expire tokens now"</button>
                                    <span class="cap">"the next call gets 190 OAuthException, subcode 463"</span></div> }.into_any()
                            }}
                            <div class="caps" style="margin-top:22px">"Webhook to the app"</div>
                            <div class="field"><label>"Callback URL"</label><input class="input mono" placeholder="https://your-app/webhooks/whatsapp (empty: the environment's callback)"
                                prop:value=s(&d["webhook"], "url") on:input=move |e| draft.update(|x| x["webhook"]["url"] = json!(event_target_value(&e)))/></div>
                            <table class="kv2">
                                <tr><td>"Verify token"</td><td class="mono">{s(&d["webhook"], "verify_token")}</td></tr>
                                <tr><td>"App secret"</td><td class="mono">{s(&d["webhook"], "app_secret")}</td></tr>
                                <tr><td>"Fields"</td><td class="mono">{d["webhook"]["fields"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("")).collect::<Vec<_>>().join("  ·  ")).unwrap_or_default()}</td></tr>
                            </table>
                            <div class="row" style="gap:10px;margin:8px 0"><Toggle on=Signal::derive(move || draft.get()["webhook"]["sign"] == true) set=move |b: bool| draft.update(|x| x["webhook"]["sign"] = json!(b))/>
                                <span>"Sign with the app secret"</span><span class="small cap mono">"X-Hub-Signature-256"</span></div>
                            <div class="row" style="gap:12px"><button class="btn" on:click=move |_| act("/whatsapp/number/webhook/verify", json!({}), "Verification request sent")>"Send the verification request"</button>
                                {(!ver.is_null()).then(|| view! { <span class=if ver["ok"] == true { "small ok" } else { "small danger" }>{if ver["ok"] == true { format!("✓ verified at {}  ·  the app echoed hub.challenge", api::hm(api::n(&ver, "at"))) } else {
                                    format!("the app answered {} at {}: it must echo hub.challenge when hub.verify_token matches", ver["status"].as_i64().map(|c| c.to_string()).unwrap_or(s(&ver, "error")), api::hm(api::n(&ver, "at"))) }}</span> })}</div>
                            <div class="caps" style="margin-top:18px">"Send a badly signed webhook"</div>
                            <div class="row" style="gap:10px;margin-top:8px">
                                <select class="input" style="width:200px" on:change=move |e| bad_kind.set(event_target_value(&e))>
                                    {[("wrong_signature", "Wrong signature"), ("missing_signature", "Missing header"), ("old_secret", "Signed with an old secret"), ("replay", "Replayed body")].into_iter().map(|(v, l)| view! { <option value=v>{l}</option> }).collect_view()}</select>
                                <button class="btn" on:click=move |_| act("/whatsapp/number/webhook/bad", json!({"kind": bad_kind.get_untracked()}), "")>"Send to the app"</button>
                                {move || { let r = bad_res.get(); (!r.is_null()).then(|| view! { <span class=if r["rejected"] == true { "small ok" } else { "small danger" }>{if r["rejected"] == true { format!("the app answered {} ✓ rejected it", r["status"]) } else { format!("the app answered {}: it accepted a forged webhook", r["status"].as_i64().map(|c| c.to_string()).unwrap_or(s(&r, "error"))) }}</span> }) }}
                            </div>
                            <div class="caps" style="margin-top:22px">"Limits and quality"</div>
                            <div class="row" style="gap:10px;margin:8px 0"><span style="width:140px">"Messaging tier"</span>{sel("/limits/tier", vec![("250", "250 numbers / 24 h"), ("1k", "1K numbers / 24 h"), ("10k", "10K numbers / 24 h"), ("100k", "100K numbers / 24 h"), ("unlimited", "Unlimited")])}<span class="small cap">"over it: 131056"</span></div>
                            <div class="row" style="gap:10px;margin:8px 0"><span style="width:140px">"Throughput"</span>{sel("/limits/throughput", vec![("1", "1 message/s"), ("20", "20 messages/s"), ("80", "80 messages/s"), ("1000", "1,000 messages/s")])}<span class="small cap">"over it: 130429 rate limit"</span></div>
                            <div class="row" style="gap:10px;margin:8px 0"><span style="width:140px">"Number quality"</span>{sel("/quality", vec![("green", "Green (high)"), ("yellow", "Yellow (medium)"), ("red", "Red (low): tier drops"), ("flagged", "Flagged"), ("restricted", "Restricted: sends fail 131048")])}<span class="small cap">"a change sends phone_number_quality_update"</span></div>
                            <div class="row" style="gap:10px;margin:8px 0"><span style="width:140px">"Template approval"</span>{sel("/approve_after", vec![("10s", "after 10 s"), ("1m", "after 1 min"), ("5m", "after 5 min"), ("1h", "after 1 h")])}<span class="small cap">"templates the app creates through the API"</span></div>
                            <div class="small cap" style="margin-top:16px">"Fields come from whatsapp/number.yaml. Secrets are references; Stuntman never shows their values here."</div>
                        }.into_any()
                    }}
                </div></div>
                <div class="side envside" style="width:420px">
                    <div class="caps">"whatsapp/number.yaml"<span class="ok" style="margin-left:8px">"live"</span></div>
                    <super::YamlMarked text=Signal::derive(move || s(&data.get(), "yaml")) before=yaml_before/>
                    <a class="link small" style="display:inline-block;margin-top:8px" target="_blank" href=api::url("/api/whatsapp/files/number.yaml")>"Open the file ›"</a>
                </div>
            </div>
        </div>
    }
}
