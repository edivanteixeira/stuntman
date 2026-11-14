//! A7 · Services › <SOAP service> › Operations: what the WSDL declares, each operation with the
//! same steps as a REST route (receive → keep → decide → respond → send); Try it runs an envelope;
//! the plugin YAML sits beside, editable by hand. REST services keep the route editor (routes.rs).

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use serde_json::{json, Map, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui,
};

/// The Routes tab: the route editor, or Operations for a SOAP service.
#[component]
pub fn RoutesOrOperations() -> impl IntoView {
    let p = use_params_map();
    let list = super::services();
    // Only the kind matters: a Memo so the live list refreshing does not remount the editor.
    let kind = Memo::new(move |_| {
        let sv = p.get().get("service").unwrap_or_default();
        let l = list.get();
        if l.is_empty() { return None; }
        Some(l.iter().find(|x| s(x, "name") == sv).map(|x| !x["soap"].is_null()).unwrap_or(false))
    });
    move || match kind.get() {
        None => view! { <div class="page"></div> }.into_any(),
        Some(true) => view! { <Operations/> }.into_any(),
        Some(false) => view! { <super::routes::Routes/> }.into_any(),
    }
}

/// A request element filled from soap.types, for Try it.
fn sample(types: &Value, ty: &str, name: &str, ns: &str, depth: usize, pad: &str) -> String {
    let xmlns = if ns.is_empty() { String::new() } else { format!(" xmlns=\"{ns}\"") };
    let Some(f) = types[ty].as_object().filter(|_| depth < 4) else {
        let v = match ty {
            "decimal" | "double" => "1250000.00".to_string(),
            "int" => "1".into(),
            "boolean" => "true".into(),
            "date" => "2026-11-14".into(),
            "dateTime" => "2026-11-14T10:00:00Z".into(),
            t if t.starts_with("enum(") => t[5..t.len() - 1].split(',').next().unwrap_or("").trim().to_string(),
            _ => format!("{}-20481", name.chars().filter(|c| c.is_ascii_alphabetic()).take(3).collect::<String>().to_uppercase()),
        };
        return format!("{pad}<{name}{xmlns}>{v}</{name}>");
    };
    let inner: Vec<String> = f.iter().filter(|(k, _)| !k.ends_with('?') && !k.ends_with('*'))
        .map(|(k, t)| sample(types, t.as_str().unwrap_or("string"), k.trim_end_matches('+'), "", depth + 1, &format!("{pad}  "))).collect();
    if inner.is_empty() { format!("{pad}<{name}{xmlns}/>") } else { format!("{pad}<{name}{xmlns}>\n{}\n{pad}</{name}>", inner.join("\n")) }
}

fn envelope(version: &str, payload: &str) -> String {
    let ns = if version == "1.1" { "http://schemas.xmlsoap.org/soap/envelope/" } else { "http://www.w3.org/2003/05/soap-envelope" };
    format!("<soap:Envelope xmlns:soap=\"{ns}\">\n  <soap:Body>\n{payload}\n  </soap:Body>\n</soap:Envelope>")
}

#[component]
pub fn Operations() -> impl IntoView {
    let p = use_params_map();
    let qm = use_query_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let plugin = RwSignal::new(Value::Null);
    let ops = RwSignal::new(Value::Null);
    let counts = RwSignal::new(Map::new());
    let sel = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let yaml_mode = RwSignal::new(false);
    let yaml_text = RwSignal::new(String::new());
    let try_env = RwSignal::new(String::new());
    let trial = RwSignal::new(Value::Null);
    let busy = RwSignal::new(false);
    let fill_try = move |op: &Value| {
        let o = ops.get_untracked();
        let ns = s(&plugin.get_untracked()["plugin"]["soap"], "namespace");
        let v = if s(&o, "version") == "1.1" { "1.1" } else { "1.2" };
        try_env.set(envelope(v, &sample(&o["types"], &s(op, "input"), &s(op, "input"), &ns, 0, "    ")));
        trial.set(Value::Null);
    };
    let load = move |pick: Option<String>| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            let (Ok(pl), Ok(o)) = (api::get(&format!("/plugins/{}", enc(&sv))).await, api::get(&format!("/plugins/{}/operations", enc(&sv))).await) else { return };
            plugin.set(pl.clone());
            ops.set(o.clone());
            if !yaml_mode.get_untracked() { yaml_text.set(s(&pl, "yaml")); }
            let list = o["operations"].as_array().cloned().unwrap_or_default();
            let want = pick.unwrap_or(sel.get_untracked());
            if draft.get_untracked() == saved.get_untracked() || saved.get_untracked().is_null() {
                if let Some(op) = list.iter().find(|x| s(x, "operation") == want).or(list.first()).cloned() {
                    sel.set(s(&op, "operation")); saved.set(op["steps"].clone()); draft.set(op["steps"].clone());
                    if try_env.get_untracked().is_empty() || want != s(&op, "operation") { fill_try(&op); }
                }
            }
            if let Ok(b) = api::get(&format!("/apis/calls?service={}&since=1d&limit=500", enc(&sv))).await {
                let mut m = Map::new();
                for c in b["calls"].as_array().cloned().unwrap_or_default() { let k = s(&c, "route"); let n = m.get(&k).and_then(|x| x.as_i64()).unwrap_or(0); m.insert(k, json!(n + 1)); }
                counts.set(m);
            }
        });
    };
    Effect::new(move |_| { service.track(); sel.set(String::new()); saved.set(Value::Null); draft.set(Value::Null); try_env.set(String::new()); load(qm.get_untracked().get("route").map(|r| r.trim_start_matches("SOAP ").to_string())); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "apis.plugin" { load(None); }
        t
    });
    let dirty = Memo::new(move |_| { let (a, b) = (saved.get(), draft.get()); (!b.is_null() && a != b) as usize });
    let save = move |_| {
        let (sv, d, op) = (service.get_untracked(), draft.get_untracked(), sel.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/operations", enc(&sv)), json!({"operation": op, "steps": d})).await {
                Ok(_) => { err.set(String::new()); saved.set(draft.get_untracked()); ui::toast("Saved in the plugin file"); load(Some(op)); }
                Err(e) => err.set(e),
            }
        });
    };
    let save_yaml = move |_| {
        let (sv, t, v) = (service.get_untracked(), yaml_text.get_untracked(), plugin.get_untracked()["version"].clone());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}", enc(&sv)), json!({"yaml": t, "version": v})).await {
                Ok(_) => { yaml_mode.set(false); saved.set(Value::Null); err.set(String::new()); ui::toast("YAML saved"); load(None); }
                Err(e) => err.set(e),
            }
        });
    };
    let reimport = move |_| {
        let sv = service.get_untracked();
        busy.set(true);
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/plugins/{}/wsdl", enc(&sv)), json!({})).await {
                Ok(v) => {
                    let (a, r) = (v["added"].as_array().map(|x| x.len()).unwrap_or(0), v["removed"].as_array().map(|x| x.len()).unwrap_or(0));
                    ui::toast(&if a + r == 0 { "The WSDL has the same operations".to_string() } else { format!("{a} operation(s) added, {r} removed") });
                    load(None);
                }
                Err(e) => ui::toast_err(e),
            }
            busy.set(false);
        });
    };
    let run_try = move |_| {
        let (sv, env) = (service.get_untracked(), try_env.get_untracked());
        let op = sel.get_untracked();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/plugins/{}/operations/try", enc(&sv)), json!({"envelope": env, "operation": op})).await {
                Ok(v) => trial.set(v),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="routes" right=move || view! {
                {move || (dirty.get() > 0).then(|| view! { <span class="small attn">"1 unsaved change"</span>
                    <button class="btn" on:click=move |_| { draft.set(saved.get_untracked()); err.set(String::new()); }>"Discard"</button> })}
                {move || if yaml_mode.get() {
                    view! { <button class="btn primary" on:click=save_yaml>"Save YAML"</button> }.into_any()
                } else {
                    view! { <button class="btn primary" disabled=move || dirty.get() == 0 on:click=save>"Save"</button> }.into_any()
                }}
            }/> }}
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">"Operations · from the WSDL"</div>
                    {move || ops.get()["operations"].as_array().cloned().unwrap_or_default().into_iter().map(|op| {
                        let name = s(&op, "operation");
                        let (n1, op2) = (name.clone(), op.clone());
                        let n = counts.get().get(&format!("SOAP {name}")).and_then(|x| x.as_i64()).unwrap_or(0);
                        let act = s(&op, "action");
                        let short = act.rsplit(['/', '#', ':']).next().unwrap_or("").to_string();
                        view! { <div class="rrow" class:on=move || sel.get() == n1 on:click=move |_| {
                                if dirty.get_untracked() > 0 { ui::toast_err("Save or discard the change first"); return; }
                                sel.set(s(&op2, "operation")); saved.set(op2["steps"].clone()); draft.set(op2["steps"].clone()); fill_try(&op2);
                            }>
                            <div class="grow"><div class="row"><span class="mono small grow">{name.clone()}</span><span class="mono small cap">{api::thousands(n)}</span></div>
                                <div class="small cap">{if act.is_empty() { "no SOAPAction: picked by its element".to_string() } else { format!("SOAPAction …/{short}") }}</div></div></div> }
                    }).collect_view()}
                    <div style="padding:16px 24px;border-top:1px solid var(--rule);margin-top:8px">
                        {move || { let o = ops.get(); view! {
                            <div class="s12" style="font-weight:500">{format!("{}  ·  SOAP {}", s(&o, "wsdl"), s(&o, "version"))}</div>
                            <div class="small cap" style="margin:2px 0 10px">{format!("endpoint {}  ·  {} types{}", s(&o, "endpoint"), o["types"].as_object().map(|t| t.len()).unwrap_or(0),
                                if s(&o, "security").is_empty() || s(&o, "security") == "none" { String::new() } else { format!("  ·  WS-Security {}", s(&o, "security")) })}</div>
                            <button class="link" style="display:block;font-size:13px" disabled=move || busy.get() on:click=reimport>"Re-import WSDL"</button>
                            <a class="link" style="display:block;font-size:13px;margin-top:6px" target="_blank" href=s(&o, "wsdlUrl")>"Open the served WSDL ›"</a>
                        } }}
                    </div>
                    <div class="small cap" style="padding:8px 24px">"An operation is what the plugin does with one SOAP action. Its request and response types come from the WSDL."</div>
                </div>
                <div class="pane pad form">
                    {move || {
                        let d = draft.get();
                        let o = ops.get();
                        if d.is_null() || o.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let answers: Vec<Value> = plugin.get()["plugin"]["answers"].as_array().cloned().unwrap_or_default();
                        let names: Vec<String> = answers.iter().map(|a| s(a, "name")).collect();
                        let op = sel.get();
                        let n_today = counts.get().get(&format!("SOAP {op}")).and_then(|x| x.as_i64()).unwrap_or(0);
                        let sec = s(&o, "security");
                        let decide = d["decide"].as_array().cloned().unwrap_or_default();
                        let sends = d["send"].as_array().cloned().unwrap_or_default();
                        let card = |n: usize, title: &'static str, body: AnyView, on: bool| view! {
                            <div class="stepcard" class:on=on><div class="row"><span class="stepn" class:on=on>{n}</span><span class="caps">{title}</span></div>{body}</div>
                        };
                        let names2 = names.clone();
                        view! {
                            <div class="input mono" style="font-size:14px;font-weight:500;width:auto;max-width:720px;display:flex;align-items:center">{op.clone()}</div>
                            <div class="row" style="margin:8px 0 14px"><span class="small attn">{format!("{} calls today", api::thousands(n_today))}</span>
                                <label class="check"><input type="checkbox" prop:checked=d["billable"] == true on:change=move |e| draft.update(|x| x["billable"] = json!(event_target_checked(&e)))/>"billable"</label></div>
                            {card(1, "Receive", view! { <div class="stepsum">{format!("SOAP {} envelope{}{}", s(&o, "version"),
                                if o["validate"] == true { format!(" checked against {}", s(&d, "input")) } else { " (types not checked)".to_string() },
                                match sec.as_str() { "any" => "  ·  WS-Security: any UsernameToken", "username_token" => "  ·  WS-Security UsernameToken", _ => "" })}</div> }.into_any(), false)}
                            {card(2, "Keep", view! {
                                <div class="small cap" style="margin-top:6px">"Save to state"</div>
                                    <input class="input mono" style="width:100%" placeholder="shipments" prop:value=s(&d["keep"], "state") on:change=move |e| { let v = event_target_value(&e); draft.update(|x| if v.trim().is_empty() { if let Some(m) = x.as_object_mut() { m.remove("keep"); } } else { x["keep"]["state"] = json!(v.trim()); }); }/>
                                    <div class="small cap" style="margin-top:6px">"by"</div>
                                    <input class="input mono" style="width:100%" placeholder="body.shipmentNumber" prop:value=s(&d["keep"], "key") on:change=move |e| { let v = event_target_value(&e); draft.update(|x| if x["keep"].is_object() { x["keep"]["key"] = json!(v.trim()); }); }/> }.into_any(), false)}
                            {card(3, "Decide", view! {
                                <div class="small cap" style="margin:6px 0">"First match wins. Fields of the request are body.<name>."</div>
                                {decide.iter().enumerate().map(|(i, r)| {
                                    let names = names2.clone();
                                    let cur = s(r, "answer");
                                    view! { <div style="margin-bottom:10px">
                                        <input class="input mono" style="width:100%" placeholder="otherwise" prop:value=s(r, "when") on:change=move |e| { let v = event_target_value(&e); draft.update(|x| if v.trim().is_empty() { if let Some(m) = x["decide"][i].as_object_mut() { m.remove("when"); } } else { x["decide"][i]["when"] = json!(v.trim()); }); }/>
                                        <div class="row" style="margin-top:4px"><span class="cap">"→"</span>
                                        <select class="select grow" on:change=move |e| draft.update(|x| x["decide"][i]["answer"] = json!(event_target_value(&e)))>
                                            {names.iter().map(|n| { let on = *n == cur; view! { <option value=n.clone() selected=on>{n.replace('_', " ")}</option> } }).collect_view()}</select>
                                        <button class="link" title="Remove the rule" on:click=move |_| draft.update(|x| { if let Some(a) = x["decide"].as_array_mut() { a.remove(i); } })>"×"</button></div></div> }
                                }).collect_view()}
                                <button class="link" style="font-size:13px" on:click={ let first = names.first().cloned().unwrap_or_default(); move |_| draft.update(|x| { let mut a = x["decide"].as_array().cloned().unwrap_or_default(); a.insert(a.len().saturating_sub(1), json!({"when": "body.", "answer": first.clone()})); x["decide"] = json!(a); }) }>"+ Add a rule"</button>
                            }.into_any(), true)}
                            {card(4, "Respond", view! {
                                <div class="stepsum">"The envelope of the named answer, or its fault"</div>
                                <div class="row" style="margin-top:6px"><span class="small cap">"latency"</span>
                                    <input class="input mono grow" placeholder="800ms" prop:value=s(&d["respond"], "latency") on:change=move |e| draft.update(|x| x["respond"]["latency"] = json!(event_target_value(&e).trim()))/></div> }.into_any(), false)}
                            {card(5, "Send", view! { <div class="stepsum">{if sends.is_empty() { "Nothing: no callback after this operation. Add one in the YAML (send: url, operation, body, delay).".to_string() } else {
                                sends.iter().map(|x| format!("{}{} to {}", if s(x, "delay").is_empty() { String::new() } else { format!("After {}: ", s(x, "delay")) }, if s(x, "operation").is_empty() { "a webhook".to_string() } else { format!("{} (SOAP)", s(x, "operation")) }, s(x, "url"))).collect::<Vec<_>>().join("\n") }}</div> }.into_any(), false)}
                            {move || (!err.get().is_empty()).then(|| view! { <div class="callout danger-l"><div class="t danger">{err.get()}</div></div> })}
                            <div class="caps" style="margin:22px 0 6px">"Try it"</div>
                            <div class="small cap">"Paste an envelope the app sent, or edit this sample. Nothing is kept, counted or sent."</div>
                            <textarea class="yaml" style="min-height:150px;margin-top:8px;max-width:720px" prop:value=move || try_env.get() on:input=move |e| try_env.set(event_target_value(&e))></textarea>
                            <div class="row" style="margin-top:8px"><button class="btn" on:click=run_try>"Run the steps"</button></div>
                            {move || { let t = trial.get(); (!t.is_null()).then(|| view! {
                                <div style="margin-top:12px;max-width:720px">
                                    <div class="row"><span class="mono small" class:danger={api::n(&t, "status") >= 400} class:ok={api::n(&t, "status") < 400}>{api::n(&t, "status")}</span><span>{s(&t, "sentence")}</span></div>
                                    {t["steps"].as_array().cloned().unwrap_or_default().into_iter().map(|x| view! { <div class="small" class:danger=x["ok"] == false class:t2=x["ok"] != false>{format!("{}  ·  {}", s(&x, "step"), s(&x, "text"))}</div> }).collect_view()}
                                    <pre class="yaml envyaml" style="max-height:220px;overflow:auto;margin-top:8px">{s(&t, "body")}</pre>
                                </div> }) }}
                        }.into_any()
                    }}
                </div>
                <div class="side yaml" style="width:430px">
                    <div class="row"><span class="caps grow">{move || s(&plugin.get(), "file").to_uppercase()}</span><span class="small ok">"live"</span>
                        <button class="link" style="margin-left:10px" on:click=move |_| { let on = !yaml_mode.get_untracked(); yaml_mode.set(on); if on { yaml_text.set(s(&plugin.get_untracked(), "yaml")); } }>{move || if yaml_mode.get() { "Back to the steps" } else { "Edit the YAML" }}</button></div>
                    {move || if yaml_mode.get() {
                        view! { <textarea class="yaml" style="min-height:70vh;margin-top:10px" prop:value=move || yaml_text.get() on:input=move |e| yaml_text.set(event_target_value(&e))></textarea> }.into_any()
                    } else {
                        view! { <pre class="yaml envyaml" style="margin-top:10px">{s(&plugin.get(), "yaml")}</pre> }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
