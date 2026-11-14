//! M2 · Rules: the same editor as S2 (rules in order, when → instead → how often, Try it), with
//! messaging's lists: operations send/receive/complete/abandon/schedule, only when property,
//! session, size, sender, delivery count; instead a delivery behaviour or a refusal.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use super::{label, Sel};
use crate::{
    api::{self, enc, s},
    ui::{self, Pop, Toggle},
};

fn blank(ns: &str, entity: &str, delivery: &str) -> Value {
    json!({"name": "", "enabled": true, "when": {"op": "receive", "namespace": ns, "entity": entity.split("/subscriptions/").next().unwrap_or(""), "subject": "", "only_when": []},
           "then": {"delivery": if delivery.is_empty() { "lose_lock" } else { delivery }}, "how_often": {"first": 1}})
}

fn often_kind(r: &Value) -> &'static str {
    if !r["how_often"]["first"].is_null() { "first" } else if !r["how_often"]["random"].is_null() { "random" } else { "every" }
}

#[component]
pub fn Rules() -> impl IntoView {
    let sel = expect_context::<Sel>();
    let qm = use_query_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| { let k = s(&sel.namespace(), "kind"); if k.is_empty() { "azure-service-bus".to_string() } else { k } });
    let data = RwSignal::new(Value::Null);
    let opts = RwSignal::new(Value::Null);
    let picked = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let trial = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let load = move |pick: Option<String>| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/services/{sv}/rules")).await {
                let rules = v["rules"].as_array().cloned().unwrap_or_default();
                let want = pick.unwrap_or(picked.get_untracked());
                let r = rules.iter().find(|r| s(r, "name") == want).or(rules.first()).cloned();
                data.set(v);
                if draft.get_untracked() == saved.get_untracked() || saved.get_untracked().is_null() {
                    let new = qm.get_untracked().get("new").unwrap_or_default();
                    if !new.is_empty() && picked.get_untracked() != "\u{0}new" && saved.get_untracked().is_null() {
                        picked.set("\u{0}new".into());
                        let q = qm.get_untracked();
                        draft.set(blank(&q.get("ns").unwrap_or(sel.ns.get_untracked()), &q.get("entity").unwrap_or(sel.entity.get_untracked()), &new));
                    } else if let Some(r) = r {
                        if picked.get_untracked() != "\u{0}new" { picked.set(s(&r, "name")); saved.set(r.clone()); draft.set(r); }
                    }
                }
            }
            if let Ok(o) = api::get(&format!("/services/{sv}/rules/options")).await { opts.set(o); }
        });
    };
    Effect::new(move |_| { service.track(); picked.set(String::new()); saved.set(Value::Null); draft.set(Value::Null); load(None); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "messaging.rules" { load(None); }
        t
    });
    Effect::new(move |_| {
        let d = draft.get();
        if d.is_null() { trial.set(Value::Null); return; }
        let sv = service.get_untracked();
        let mut body = json!({"rule": if s(&d, "name").is_empty() { "draft".to_string() } else { s(&d, "name") }, "when": d["when"], "instead": d["then"], "how_often": d["how_often"]});
        if let Some(w) = body["when"].as_object_mut() { w.retain(|_, v| !(v.is_string() && v.as_str() == Some(""))); }
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/services/{sv}/rules/test"), body).await { Ok(v) => { trial.set(v); err.set(String::new()); } Err(e) => { trial.set(Value::Null); err.set(e); } }
        });
    });
    let dirty = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        if a.is_null() { return if b.is_null() { 0 } else { 1 }; }
        ["/name", "/when", "/then", "/how_often"].iter().filter(|p| a.pointer(p) != b.pointer(p)).count()
    });
    let set = move |p: &'static str, v: Value| draft.update(|d| { if let Some(x) = d.pointer_mut(p) { *x = v; } else if let Some((parent, k)) = p.rsplit_once('/') { if let Some(o) = d.pointer_mut(parent).and_then(|x| x.as_object_mut()) { o.insert(k.to_string(), v); } } });
    let save = move |_| {
        let (sv, d, old) = (service.get_untracked(), draft.get_untracked(), saved.get_untracked());
        let mut when = d["when"].clone();
        if let Some(w) = when.as_object_mut() { w.retain(|_, v| !(v.is_string() && v.as_str() == Some(""))); }
        leptos::task::spawn_local(async move {
            let r = if old.is_null() {
                api::send("POST", &format!("/services/{sv}/rules"), json!({"rule": s(&d, "name"), "when": when, "instead": d["then"], "how_often": d["how_often"], "enabled": true})).await
            } else {
                let mut b = json!({"when": when, "instead": d["then"], "how_often": d["how_often"]});
                if s(&d, "name") != s(&old, "name") { b["new_name"] = d["name"].clone(); }
                api::send("PATCH", &format!("/services/{sv}/rules/{}", enc(&s(&old, "name"))), b).await
            };
            match r {
                Ok(v) => { ui::toast("Saved; the count starts again"); picked.set(s(&v, "name")); saved.set(v.clone()); draft.set(v); load(Some(s(&d, "name"))); }
                Err(e) => err.set(e),
            }
        });
    };
    let pick = move |name: String| {
        let r = data.get_untracked()["rules"].as_array().and_then(|x| x.iter().find(|r| s(r, "name") == name).cloned());
        if let Some(r) = r { picked.set(name); saved.set(r.clone()); draft.set(r); }
    };
    let act = move |m: &'static str, path: String, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        match api::send(m, &path, body).await { Ok(_) => ui::toast(done), Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="body">
            <div class="list narrow tree">
                <div class="caps section" style="padding:18px 24px 8px">"Rules · run in this order"</div>
                {move || {
                    let rules = data.get()["rules"].as_array().cloned().unwrap_or_default();
                    let n = rules.len();
                    rules.into_iter().enumerate().map(|(i, r)| {
                        let name = s(&r, "name");
                        let (n1, n2, n3, n4, n5, n6) = (name.clone(), name.clone(), name.clone(), name.clone(), name.clone(), name.clone());
                        let menu = RwSignal::new(false);
                        let on = r["enabled"] == true;
                        let sv = service.get();
                        let (s1, s3, s4, s5) = (sv.clone(), sv.clone(), sv.clone(), sv.clone());
                        let warn = r["warnings"].as_array().map(|w| !w.is_empty()).unwrap_or(false);
                        let r2 = r.clone();
                        view! {
                            <div class="rrow" class:on=move || picked.get() == n1>
                                <Toggle on=on set=move |v: bool| act("PUT", format!("/services/{s1}/rules/{}/enabled", enc(&n2)), json!({"enabled": v}), if v { "On; the count starts again" } else { "Off; it stays in the YAML" })/>
                                <div class="grow" on:click=move |_| pick(n3.clone())>
                                    <div class:cap=!on>{name.clone()}</div>
                                    <div class="mono small cap">{s(&r2, "meta")}</div>
                                    <div class="small" class:attn={api::n(&r2, "hitsToday") > 0 || warn} class:cap={api::n(&r2, "hitsToday") == 0 && !warn}>{if !on { "off".to_string() } else if warn { "never runs: a rule above answers first".into() } else { format!("hit {} today", api::n(&r2, "hitsToday")) }}</div>
                                </div>
                                <span class="anchor"><button class="link plain" on:click=move |_| menu.set(true)>"⋯"</button>
                                    <Pop open=menu right=true>
                                        {(i > 0).then(|| { let (s3, n4) = (s3.clone(), n4.clone()); view! { <div class="mi" on:click=move |_| act("PUT", format!("/services/{s3}/rules/{}/position", enc(&n4)), json!({"position": i}), "Moved up")>"Move up"</div> } })}
                                        {(i + 1 < n).then(|| { let (s4, n5) = (s4.clone(), n5.clone()); view! { <div class="mi" on:click=move |_| act("PUT", format!("/services/{s4}/rules/{}/position", enc(&n5)), json!({"position": i + 2}), "Moved down")>"Move down"</div> } })}
                                        <div class="rule"></div>
                                        <div class="mi danger" on:click={ let (s5, n6) = (s5.clone(), n6.clone()); move |_| act("DELETE", format!("/services/{s5}/rules/{}", enc(&n6)), json!({}), "Rule deleted") }>"Delete rule"</div>
                                    </Pop></span>
                            </div>
                        }
                    }).collect_view()
                }}
                <button class="link add" on:click=move |_| { picked.set("\u{0}new".into()); saved.set(Value::Null); draft.set(blank(&sel.ns.get_untracked(), &sel.entity.get_untracked(), "")); }>"+ New rule"</button>
                <div class="small cap" style="padding:8px 24px">{move || format!("Rules of {} in this environment. The first rule that matches answers. A rule never touches other environments.", label(&service.get()))}</div>
            </div>
            <div class="pane pad form">
                {move || {
                    let d = draft.get();
                    if d.is_null() {
                        return view! { <div class="callout"><div class="t">{format!("No rules for {} yet", label(&service.get()))}</div>
                            <div class="t2">"A rule keeps a delivery behaviour or a refusal on until you switch it off. For a one-off, use “Next delivery” on the Messages tab."</div>
                            <button class="btn" on:click=move |_| { picked.set("\u{0}new".into()); draft.set(blank(&sel.ns.get_untracked(), &sel.entity.get_untracked(), "")); }>"New rule"</button></div> }.into_any();
                    }
                    let o = opts.get();
                    let nss = o["namespaces"].as_array().cloned().unwrap_or_default();
                    let nsv = s(&d["when"], "namespace");
                    let ents: Vec<String> = nss.iter().filter(|x| nsv.is_empty() || s(x, "namespace") == nsv).flat_map(|x| x["entities"].as_array().cloned().unwrap_or_default().into_iter().map(|e| e.as_str().unwrap_or("").to_string())).collect();
                    let delivery = s(&d["then"], "delivery");
                    let refuse = s(&d["then"], "refuse");
                    let instead = if delivery.is_empty() { format!("refuse:{refuse}") } else { format!("delivery:{delivery}") };
                    let kind = often_kind(&d);
                    let ow = d["when"]["only_when"].as_array().cloned().unwrap_or_default();
                    view! {
                        <div class="row" style="justify-content:flex-end;gap:10px">
                            {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span> }) }}
                            {move || (dirty.get() > 0).then(|| view! { <button class="btn" on:click=move |_| { if saved.get_untracked().is_null() { draft.set(Value::Null); picked.set(String::new()); load(None); } else { draft.set(saved.get_untracked()) } }>"Discard"</button> })}
                            <button class="btn primary" disabled=move || dirty.get() == 0 || s(&draft.get(), "name").trim().is_empty() on:click=save>"Save"</button></div>
                        <div class="field"><label>"Name"</label>
                            <input class="input" style="width:320px" placeholder="Orders lose the lock" prop:value=s(&d, "name") on:input=move |e| set("/name", json!(event_target_value(&e)))/></div>
                        <div class="caps" style="margin:20px 0 8px">"When"</div>
                        <div class="grid3">
                            <div class="field"><label>"Operation"</label><select class="select" on:change=move |e| set("/when/op", json!(event_target_value(&e)))>
                                {o["operations"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let v = s(&x, "value"); let on = v == s(&d["when"], "op"); view! { <option value=v selected=on>{s(&x, "label")}</option> } }).collect_view()}</select></div>
                            <div class="field"><label>"Namespace"</label><select class="select" on:change=move |e| { set("/when/namespace", json!(event_target_value(&e))); set("/when/entity", json!("")); }>
                                <option value="" selected=nsv.is_empty()>"Any namespace"</option>
                                {nss.iter().map(|x| { let v = s(x, "namespace"); let on = v == nsv; view! { <option value=v.clone() selected=on>{v.clone()}</option> } }).collect_view()}</select></div>
                            <div class="field"><label>"Entity"</label><select class="select" on:change=move |e| set("/when/entity", json!(event_target_value(&e)))>
                                <option value="" selected=s(&d["when"], "entity").is_empty()>"Any"</option>
                                {ents.into_iter().map(|c| { let on = c == s(&d["when"], "entity"); view! { <option value=c.clone() selected=on>{c.clone()}</option> } }).collect_view()}</select></div>
                        </div>
                        <div class="field" style="margin-top:14px"><label>"Subject matches"</label>
                            <input class="input mono" placeholder="OrderCreated" prop:value=s(&d["when"], "subject") on:input=move |e| set("/when/subject", json!(event_target_value(&e)))/>
                            <div class="small cap">"Exact text or a pattern like Order*. Leave empty for every message."</div></div>
                        {ow.iter().enumerate().map(|(i, c)| {
                            let c = c.clone();
                            let field = s(&c, "field");
                            view! { <div class="row" style="margin-top:8px;gap:8px">
                                <select class="select" on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["field"] = json!(event_target_value(&e)))>
                                    {[("property", "Property"), ("session", "Session id"), ("size", "Size (KB) over"), ("sender", "Sender is"), ("delivery_count", "Delivery count at least")].into_iter().map(|(v, l)| { let on = field == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                {(field == "property").then(|| view! { <input class="input mono" style="width:120px" placeholder="tenant" prop:value=s(&c, "key") on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["key"] = json!(event_target_value(&e)))/><span class="small cap">"equals"</span> })}
                                <input class="input mono" style="width:160px" prop:value=s(&c, "value") on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["value"] = json!(event_target_value(&e)))/>
                                <button class="link" on:click=move |_| draft.update(|d| { if let Some(a) = d["when"]["only_when"].as_array_mut() { a.remove(i); } })>"Remove"</button></div> }
                        }).collect_view()}
                        <button class="link" style="margin-top:10px;font-size:13px" on:click=move |_| draft.update(|d| { if !d["when"]["only_when"].is_array() { d["when"]["only_when"] = json!([]); } d["when"]["only_when"].as_array_mut().unwrap().push(json!({"field": "property", "key": "tenant", "value": ""})); })>
                            {if ow.is_empty() { "+ Only when…  (property, session, size, sender, delivery count)" } else { "+ Another condition  ·  all must be true" }}</button>
                        <div class="caps" style="margin:22px 0 8px">"Then"</div>
                        <div class="row"><span style="width:120px">"Instead"</span>
                            <select class="select" on:change=move |e| { let v = event_target_value(&e); let (k, x) = v.split_once(':').unwrap_or(("delivery", "lose_lock"));
                                draft.update(|d| { d["then"] = if k == "delivery" { json!({"delivery": x}) } else { json!({"refuse": x}) }; }); }>
                                <optgroup label="Delivery">{o["instead"]["delivery"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let v = format!("delivery:{}", s(&x, "value")); let on = v == instead; view! { <option value=v selected=on>{s(&x, "label")}</option> } }).collect_view()}</optgroup>
                                <optgroup label="Refuse">{o["instead"]["refuse"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let v = format!("refuse:{}", s(&x, "value")); let on = v == instead; view! { <option value=v selected=on>{s(&x, "label")}</option> } }).collect_view()}</optgroup>
                            </select>
                            {(delivery == "hold").then(|| view! { <input class="input mono" style="width:70px" prop:value=d["then"]["seconds"].as_u64().unwrap_or(30).to_string() on:input=move |e| set("/then/seconds", json!(event_target_value(&e).parse::<u64>().unwrap_or(30)))/><span class="small cap">"seconds"</span> })}
                        </div>
                        <div class="row" style="margin-top:16px;align-items:flex-start"><span style="width:120px;padding-top:6px">"How often"</span>
                            <div class="col" style="gap:8px">
                                <label class="radio" class:on=kind == "first" on:click=move |_| if often_kind(&draft.get_untracked()) != "first" { set("/how_often", json!({"first": 1})) }><span class="r"></span>"The first"
                                    <input class="input mono" style="width:56px" disabled=kind != "first" prop:value=d["how_often"]["first"].as_u64().unwrap_or(1).to_string() on:input=move |e| set("/how_often", json!({"first": event_target_value(&e).parse::<u64>().unwrap_or(1)}))/>"deliveries, then normal"</label>
                                <label class="radio" class:on=kind == "every" on:click=move |_| set("/how_often", json!({}))><span class="r"></span>"Every time"</label>
                                <label class="radio" class:on=kind == "random" on:click=move |_| if often_kind(&draft.get_untracked()) != "random" { set("/how_often", json!({"random": 20})) }><span class="r"></span>"Randomly,"
                                    <input class="input mono" style="width:56px" disabled=kind != "random" prop:value=d["how_often"]["random"].as_u64().unwrap_or(20).to_string() on:input=move |e| set("/how_often", json!({"random": event_target_value(&e).parse::<u64>().unwrap_or(20)}))/>"% of the deliveries"</label>
                            </div></div>
                        <div class="small cap" style="margin-top:14px">"Count starts again when the rule is saved or switched on."</div>
                        {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:10px">{err.get()}</div> })}
                        <div class="row" style="margin-top:20px;border-top:1px solid var(--rule);padding-top:14px">
                            <span class="small cap">{format!("Saved in messaging/rules/{}.yaml for {}  ·  ", service.get(), s(&expect_context::<RwSignal<Value>>().get(), "env").to_uppercase())}</span>
                            <a class="link" href=api::url(&format!("/api/messaging/rules/{}", service.get())) target="_blank">"View YAML"</a></div>
                    }.into_any()
                }}
            </div>
            <div class="side envside">
                {move || {
                    let t = trial.get();
                    if t.is_null() { return view! { <div class="cap">"Try it runs as you edit."</div> }.into_any(); }
                    let (m, of) = (api::n(&t, "matched"), api::n(&t, "of"));
                    let pct = if of > 0 { m * 100 / of } else { 0 };
                    view! {
                        <div class="caps">"Try it"</div>
                        <div style="margin:8px 0 6px">{format!("Against the last {of} deliveries on {}", label(&service.get()))}</div>
                        <div class="bar"><div class="m" style=format!("width:{pct}%;background:#a8740a")></div></div>
                        <div class="small cap">{format!("{m} would match  ·  {} untouched", api::n(&t, "untouched"))}</div>
                        {t["warnings"].as_array().cloned().unwrap_or_default().into_iter().map(|w| view! { <div class="callout attn-l"><div class="t attn">"This rule never runs"</div><div class="t2">{s(&w, "text")}</div></div> }).collect_view()}
                        {(m > 0).then(|| view! {
                            <div class="caps" style="margin:18px 0 8px">"Would match"</div>
                            {t["wouldMatch"].as_array().cloned().unwrap_or_default().into_iter().take(6).map(|x| view! {
                                <div style="margin-bottom:8px"><div class="mono small">{format!("#{}  {}  ·  {}", api::n(&x, "seq"), s(&x, "subject"), s(&x, "entity"))}</div>
                                    <div class="small cap">{format!("{}  ·  {}", api::hm(api::n(&x, "at")), if s(&x, "consumer").is_empty() { s(&x, "sender") } else { s(&x, "consumer") })}</div></div> }).collect_view()}
                        })}
                        <div class="caps" style="margin:18px 0 8px">"What the app will see"</div>
                        {t["appWillSee"].as_array().cloned().unwrap_or_default().into_iter().map(|a| {
                            let n = api::n(&a, "try");
                            let lbl = match n { 0 => "Each delivery".to_string(), 1 => "1st delivery".into(), 2 => "2nd delivery".into(), 3 => "3rd delivery".into(), k => format!("{k}th delivery") };
                            let p = a["provoked"] == true;
                            view! { <div class="row" style="margin-bottom:6px"><span style="width:100px">{lbl}</span><span class="mono small" class:attn=p class:ok=!p>{s(&a, "answer")}</span></div> }
                        }).collect_view()}
                        <div class="small cap" style="margin-top:14px">"Changes here are only tried, never sent, until you save."</div>
                    }.into_any()
                }}
            </div>
        </div>
    }
}
