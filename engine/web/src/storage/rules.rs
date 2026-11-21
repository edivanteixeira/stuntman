//! S2 · Services › Azure Blob (or S3) › Rules: the rule list in order, the editor (when → then →
//! how often), and Try it against the last 50 requests with what the app will see.

use leptos::prelude::*;
use leptos_router::hooks::{use_location, use_query_map};
use serde_json::{json, Value};

use super::{service_label, ServiceHead};
use crate::{
    api::{self, enc, s},
    ui::{self, Pop, Toggle},
};

fn blank(service: &str, path: &str) -> Value {
    let _ = service;
    json!({"name": "", "enabled": true, "when": {"op": "put", "account": "", "container": "", "path": path, "only_when": []}, "then": {"respond": "503"}, "how_often": {"first": 2}})
}

fn often_kind(r: &Value) -> &'static str {
    if !r["how_often"]["first"].is_null() { "first" } else if !r["how_often"]["random"].is_null() { "random" } else { "every" }
}

#[component]
pub fn Rules() -> impl IntoView {
    let loc = use_location();
    let qm = use_query_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| loc.pathname.get().split('/').nth(2).unwrap_or("azure-blob").to_string());
    let data = RwSignal::new(Value::Null);
    let opts = RwSignal::new(Value::Null);
    let sel = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let trial = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let confirm_leave = RwSignal::new(None::<String>);
    let leave_open = RwSignal::new(false);
    let load = move |pick: Option<String>| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/services/{sv}/rules")).await {
                let rules = v["rules"].as_array().cloned().unwrap_or_default();
                let want = pick.unwrap_or(sel.get_untracked());
                let r = rules.iter().find(|r| s(r, "name") == want).or(rules.first()).cloned();
                data.set(v);
                if draft.get_untracked() == saved.get_untracked() || saved.get_untracked().is_null() {
                    match r {
                        Some(r) => { sel.set(s(&r, "name")); saved.set(r.clone()); draft.set(r); }
                        None if sel.get_untracked() != "\u{0}new" => {
                            let path = qm.get_untracked().get("path").unwrap_or_default();
                            if !path.is_empty() { sel.set("\u{0}new".into()); let b = blank(&sv, &path); saved.set(Value::Null); draft.set(b); }
                        }
                        None => {}
                    }
                }
            }
            if let Ok(o) = api::get(&format!("/services/{sv}/rules/options")).await { opts.set(o); }
        });
    };
    Effect::new(move |_| { service.track(); sel.set(String::new()); saved.set(Value::Null); draft.set(Value::Null); load(None); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "storage.rules" { load(None); }
        t
    });
    // Try it: any change runs the test, nothing is saved.
    Effect::new(move |_| {
        let d = draft.get();
        if d.is_null() { trial.set(Value::Null); return; }
        let sv = service.get_untracked();
        let mut body = json!({"rule": if s(&d, "name").is_empty() { "draft".to_string() } else { s(&d, "name") }, "when": d["when"], "then": d["then"], "how_often": d["how_often"]});
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
                api::send("POST", &format!("/services/{sv}/rules"), json!({"rule": s(&d, "name"), "when": when, "then": d["then"], "how_often": d["how_often"], "enabled": d["enabled"]})).await
            } else {
                let mut b = json!({"when": when, "then": d["then"], "how_often": d["how_often"]});
                if s(&d, "name") != s(&old, "name") { b["new_name"] = d["name"].clone(); }
                api::send("PATCH", &format!("/services/{sv}/rules/{}", enc(&s(&old, "name"))), b).await
            };
            match r {
                Ok(v) => { ui::toast("Saved; the count starts again"); sel.set(s(&v, "name")); saved.set(v.clone()); draft.set(v); load(Some(s(&d, "name"))); }
                Err(e) => err.set(e),
            }
        });
    };
    let pick = move |name: String| {
        if dirty.get_untracked() > 0 { confirm_leave.set(Some(name)); leave_open.set(true); return; }
        let r = data.get_untracked()["rules"].as_array().and_then(|x| x.iter().find(|r| s(r, "name") == name).cloned());
        if let Some(r) = r { sel.set(name); saved.set(r.clone()); draft.set(r); }
    };
    let act = move |m: &'static str, path: String, body: Value, done: &'static str| leptos::task::spawn_local(async move {
        match api::send(m, &path, body).await { Ok(_) => ui::toast(done), Err(e) => ui::toast_err(e) }
    });
    view! {
        <div class="page">
            {move || view! { <ServiceHead service=service.get() active="rules" right=move || view! {
                {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| { if saved.get_untracked().is_null() { draft.set(Value::Null); sel.set(String::new()); load(None); } else { draft.set(saved.get_untracked()) } }>"Discard"</button> }) }}
                <button class="btn primary" disabled=move || dirty.get() == 0 || s(&draft.get(), "name").trim().is_empty() on:click=save>"Save"</button>
            }/> }}
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
                            let (s1, s2, s3, s4, s5) = (sv.clone(), sv.clone(), sv.clone(), sv.clone(), sv.clone());
                            let warn = r["warnings"].as_array().map(|w| !w.is_empty()).unwrap_or(false);
                            let r2 = r.clone();
                            view! {
                                <div class="rrow" class:on=move || sel.get() == n1>
                                    <Toggle on=on set=move |v: bool| act("PUT", format!("/services/{s1}/rules/{}/enabled", enc(&n2)), json!({"enabled": v}), if v { "On; the count starts again" } else { "Off; it stays in the YAML" })/>
                                    <div class="grow" on:click=move |_| pick(n3.clone())>
                                        <div class:cap=!on>{name.clone()}</div>
                                        <div class="mono small cap">{s(&r2, "meta")}</div>
                                        <div class="small" class:attn={api::n(&r2, "hitsToday") > 0 || warn} class:cap={api::n(&r2, "hitsToday") == 0 && !warn}>{if !on { "off".to_string() } else if warn { "never runs: a rule above answers first".into() } else { format!("hit {} today", api::n(&r2, "hitsToday")) }}</div>
                                    </div>
                                    <span class="anchor"><button class="link plain" on:click=move |_| menu.set(true)>"⋯"</button>
                                        <Pop open=menu right=true>
                                            <div class="mi" on:click={ let (r, s2) = (r.clone(), s2.clone()); move |_| act("POST", format!("/services/{s2}/rules"), json!({"rule": format!("{} (copy)", s(&r, "name")), "when": r["when"], "then": r["then"], "how_often": r["how_often"], "enabled": false}), "Duplicated (off)") }>"Duplicate"</div>
                                            {(i > 0).then(|| { let (s3, n4) = (s3.clone(), n4.clone()); view! { <div class="mi" on:click=move |_| act("PUT", format!("/services/{s3}/rules/{}/position", enc(&n4)), json!({"position": i}), "Moved up")>"Move up"</div> } })}
                                            {(i + 1 < n).then(|| { let (s4, n5) = (s4.clone(), n5.clone()); view! { <div class="mi" on:click=move |_| act("PUT", format!("/services/{s4}/rules/{}/position", enc(&n5)), json!({"position": i + 2}), "Moved down")>"Move down"</div> } })}
                                            <div class="rule"></div>
                                            <div class="mi danger" on:click={ let (s5, n6) = (s5.clone(), n6.clone()); move |_| act("DELETE", format!("/services/{s5}/rules/{}", enc(&n6)), json!({}), "Rule deleted") }>"Delete rule"</div>
                                        </Pop></span>
                                </div>
                            }
                        }).collect_view()
                    }}
                    <button class="link add" on:click=move |_| { sel.set("\u{0}new".into()); saved.set(Value::Null); draft.set(blank(&service.get_untracked(), "")); }>"+ New rule"</button>
                    <div class="small cap" style="padding:8px 24px">"The first rule that matches answers. A rule never touches other environments."</div>
                </div>
                <div class="pane pad form">
                    {move || {
                        let d = draft.get();
                        if d.is_null() {
                            return view! { <div class="callout"><div class="t">{format!("No rules for {} yet", service_label(&service.get()))}</div>
                                <div class="t2">"A rule keeps a failure on until you switch it off. For a one-off failure, use “Next read” on a file in Files."</div>
                                <button class="btn" on:click=move |_| { sel.set("\u{0}new".into()); draft.set(blank(&service.get_untracked(), "")); }>"New rule"</button></div> }.into_any();
                        }
                        let o = opts.get();
                        let accts = o["accounts"].as_array().cloned().unwrap_or_default();
                        let acct = s(&d["when"], "account");
                        let conts: Vec<String> = accts.iter().filter(|a| acct.is_empty() || s(a, "account") == acct).flat_map(|a| a["containers"].as_array().cloned().unwrap_or_default().into_iter().map(|c| c.as_str().unwrap_or("").to_string())).collect();
                        let resp = s(&d["then"], "respond");
                        let kind = often_kind(&d);
                        let ow = d["when"]["only_when"].as_array().cloned().unwrap_or_default();
                        view! {
                            <div class="field"><label>"Name"</label>
                                <div class="row"><input class="input" style="width:300px" prop:value=s(&d, "name") on:input=move |e| set("/name", json!(event_target_value(&e)))/>
                                    <span class="small cap">{move || { let r = data.get()["rules"].as_array().and_then(|x| x.iter().find(|r| r["name"] == draft.get()["name"]).cloned()).unwrap_or(Value::Null);
                                        if r.is_null() { "new rule".to_string() } else { format!("{} hits today{}", api::n(&r, "hitsToday"), if r["lastHit"].is_null() { String::new() } else { format!("  ·  last hit {}", api::hm(api::n(&r, "lastHit"))) }) } }}</span></div></div>
                            <div class="caps" style="margin:20px 0 8px">"When"</div>
                            <div class="grid3">
                                <div class="field"><label>"Operation"</label><select class="select" on:change=move |e| set("/when/op", json!(event_target_value(&e)))>
                                    {o["operations"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let v = s(&x, "value"); let on = v == s(&d["when"], "op"); view! { <option value=v selected=on>{s(&x, "label")}</option> } }).collect_view()}</select></div>
                                <div class="field"><label>"Account"</label><select class="select" on:change=move |e| { set("/when/account", json!(event_target_value(&e))); set("/when/container", json!("")); }>
                                    <option value="" selected=acct.is_empty()>"Any account"</option>
                                    {accts.iter().map(|a| { let v = s(a, "account"); let on = v == acct; view! { <option value=v.clone() selected=on>{v.clone()}</option> } }).collect_view()}</select></div>
                                <div class="field"><label>{if service.get() == "s3" { "Bucket" } else { "Container" }}</label><select class="select" on:change=move |e| set("/when/container", json!(event_target_value(&e)))>
                                    <option value="" selected=s(&d["when"], "container").is_empty()>"Any"</option>
                                    {conts.into_iter().map(|c| { let on = c == s(&d["when"], "container"); view! { <option value=c.clone() selected=on>{c.clone()}</option> } }).collect_view()}</select></div>
                            </div>
                            <div class="field" style="margin-top:14px"><label>"Path matches"</label>
                                <input class="input mono" placeholder="contracts/**" prop:value=s(&d["when"], "path") on:input=move |e| set("/when/path", json!(event_target_value(&e)))/>
                                <div class="small cap">"Glob: * one level, ** any depth. Leave empty for the whole container."</div></div>
                            {ow.iter().enumerate().map(|(i, c)| {
                                let c = c.clone();
                                view! { <div class="row" style="margin-top:8px">
                                    <select class="select" on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["field"] = json!(event_target_value(&e)))>
                                        {[("size", "Size"), ("content_type", "Content type"), ("caller", "Caller")].into_iter().map(|(v, l)| { let on = s(&c, "field") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                    <select class="select" on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["op"] = json!(event_target_value(&e)))>
                                        {[("over", "is over"), ("under", "is under"), ("is", "is"), ("is_not", "is not"), ("contains", "contains")].into_iter().map(|(v, l)| { let on = s(&c, "op") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select>
                                    <input class="input mono" style="width:200px" prop:value=s(&c, "value") on:change=move |e| draft.update(|d| d["when"]["only_when"][i]["value"] = json!(event_target_value(&e)))/>
                                    <button class="link" on:click=move |_| draft.update(|d| { if let Some(a) = d["when"]["only_when"].as_array_mut() { a.remove(i); } })>"Remove"</button></div> }
                            }).collect_view()}
                            <button class="link" style="margin-top:10px;font-size:13px" on:click=move |_| draft.update(|d| { if !d["when"]["only_when"].is_array() { d["when"]["only_when"] = json!([]); } d["when"]["only_when"].as_array_mut().unwrap().push(json!({"field": "size", "op": "over", "value": "10 MB"})); })>
                                {if ow.is_empty() { "+ Only when…  (size, content type, caller)" } else { "+ Another condition  ·  all must be true" }}</button>
                            <div class="caps" style="margin:22px 0 8px">"Then"</div>
                            <div class="row"><span style="width:120px">"Respond with"</span>
                                <select class="select" on:change=move |e| set("/then/respond", json!(event_target_value(&e)))>
                                    {["Errors", "Network", "Content", "Links"].into_iter().map(|g| view! { <optgroup label=g>
                                        {o["responses"].as_array().cloned().unwrap_or_default().into_iter().filter(|r| s(r, "group") == g).map(|r| { let v = s(&r, "value"); let on = v == resp; view! { <option value=v selected=on>{s(&r, "label")}</option> } }).collect_view()}
                                    </optgroup> }).collect_view()}</select>
                                {(resp == "slow" || resp == "expires").then(|| view! { <input class="input mono" style="width:70px" prop:value=d["then"]["seconds"].as_u64().unwrap_or(5).to_string() on:input=move |e| set("/then/seconds", json!(event_target_value(&e).parse::<u64>().unwrap_or(5)))/><span class="small cap">"seconds"</span> })}
                            </div>
                            <div class="row" style="margin-top:16px;align-items:flex-start"><span style="width:120px;padding-top:6px">"How often"</span>
                                <div class="col" style="gap:8px">
                                    <label class="radio" class:on=kind == "first" on:click=move |_| set("/how_often", json!({"first": 2}))><span class="r"></span>"The first"
                                        <input class="input mono" style="width:56px" disabled=kind != "first" prop:value=d["how_often"]["first"].as_u64().unwrap_or(2).to_string() on:input=move |e| set("/how_often", json!({"first": event_target_value(&e).parse::<u64>().unwrap_or(1)}))/>"attempts, then normal"</label>
                                    <label class="radio" class:on=kind == "every" on:click=move |_| set("/how_often", json!({}))><span class="r"></span>"Every time"</label>
                                    <label class="radio" class:on=kind == "random" on:click=move |_| if often_kind(&draft.get_untracked()) != "random" { set("/how_often", json!({"random": 20})) }><span class="r"></span>"Randomly,"
                                        <input class="input mono" style="width:56px" disabled=kind != "random" prop:value=d["how_often"]["random"].as_u64().unwrap_or(20).to_string() on:input=move |e| set("/how_often", json!({"random": event_target_value(&e).parse::<u64>().unwrap_or(20)}))/>"% of the requests"</label>
                                    {(kind == "random").then(|| view! { <div class="small cap">{format!("About 1 in {} fails. The rest pass untouched.", (100 / d["how_often"]["random"].as_u64().unwrap_or(20).max(1)).max(1))}</div> })}
                                </div></div>
                            <div class="small cap" style="margin-top:14px">"Count starts again when the rule is saved or switched on."</div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="err" style="margin-top:10px">{err.get()}</div> })}
                            <div class="row" style="margin-top:20px;border-top:1px solid var(--rule);padding-top:14px">
                                <span class="small cap">{format!("Saved in storage/rules/{}.yaml for {}  ·  ", service.get(), s(&expect_context::<RwSignal<Value>>().get(), "env").to_uppercase())}</span>
                                <a class="link" href=api::url(&format!("/api/services/{}/rules.yaml", service.get())) target="_blank">"View YAML"</a></div>
                        }.into_any()
                    }}
                </div>
                <div class="side envside">
                    {move || {
                        let t = trial.get();
                        if t.is_null() { return view! { <div class="cap">"Try it runs as you edit."</div> }.into_any(); }
                        let (m, of) = (api::n(&t, "matched"), api::n(&t, "of"));
                        let pct = if of > 0 { m * 100 / of } else { 0 };
                        let near = t["nearMiss"].clone();
                        view! {
                            <div class="caps">"Try it"</div>
                            <div style="margin:8px 0 6px">{format!("Against the last {of} requests to {}", service_label(&service.get()))}</div>
                            <div class="bar"><div class="m" style=format!("width:{pct}%;background:var(--paused)")></div></div>
                            <div class="small cap">{format!("{m} would match  ·  {} untouched", api::n(&t, "untouched"))}</div>
                            {t["warnings"].as_array().cloned().unwrap_or_default().into_iter().map(|w| view! {
                                <div class="callout attn-l"><div class="t attn">"This rule never runs"</div><div class="t2">{s(&w, "text")}</div></div> }).collect_view()}
                            {(m > 0).then(|| view! {
                                <div class="caps" style="margin:18px 0 8px">"Would match"</div>
                                {t["wouldMatch"].as_array().cloned().unwrap_or_default().into_iter().take(6).map(|x| view! {
                                    <div style="margin-bottom:8px"><div class="mono small">{s(&x, "path")}</div><div class="small cap">{format!("{}  ·  {}  ·  {}", s(&x, "method"), api::hm(api::n(&x, "at")), s(&x, "caller"))}</div></div> }).collect_view()}
                            })}
                            {(m == 0 && of > 0).then(|| view! { <div class="callout"><div class="t">{format!("No request in the last {of} matches")}</div>
                                {(!near.is_null()).then(|| { let fix = s(&near, "fix"); view! {
                                    <div class="t2">"Closest one:"</div><div class="mono small">{s(&near, "path")}</div>
                                    <div class="small attn">{format!("“{}/” here, “{}/” in your rule.", s(&near, "here"), s(&near, "yours"))}</div>
                                    <button class="link" on:click=move |_| set("/when/path", json!(fix.clone()))>{format!("Use “{}” instead", s(&near, "fix"))}</button> } })}</div> })}
                            <div class="caps" style="margin:18px 0 8px">"What the app will see"</div>
                            {t["appWillSee"].as_array().cloned().unwrap_or_default().into_iter().map(|a| {
                                let n = api::n(&a, "try");
                                let label = match n { 0 => "Each try".to_string(), 1 => "1st try".into(), 2 => "2nd try".into(), 3 => "3rd try".into(), k => format!("{k}th try") };
                                let p = a["provoked"] == true;
                                view! { <div class="row" style="margin-bottom:6px"><span style="width:70px">{label}</span><span class="mono small" class:attn=p class:ok=!p>{s(&a, "answer")}</span></div> }
                            }).collect_view()}
                            <div class="small cap" style="margin-top:14px">"Changes here are only tried, never sent, until you save."</div>
                        }.into_any()
                    }}
                </div>
            </div>
            <ui::Confirm open=leave_open question=Signal::derive(move || format!("Discard {} change{} to this rule?", dirty.get(), if dirty.get() == 1 { "" } else { "s" }))
                detail=Signal::derive(|| "The rule keeps running as it was saved.".to_string()) action="Discard"
                on_yes=move |_| { let n = confirm_leave.get_untracked().unwrap_or_default(); confirm_leave.set(None); draft.set(saved.get_untracked()); pick(n); }/>
        </div>
    }
}
