//! A3 · Services › <plugin> › Routes: the route editor. Steps receive → let → keep → wait →
//! decide → respond → send, each a card (the selected one opens its fields); Try it runs a
//! sample without storing or billing; the plugin YAML sits beside, editable by hand.

use leptos::prelude::*;
use leptos_router::hooks::{use_params_map, use_query_map};
use serde_json::{json, Value};

use super::PluginHead;
use crate::{
    api::{self, enc, s},
    ui::{self, Pop},
};

fn list_text(v: &Value) -> String {
    v.as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
}

fn parse_list(t: &str) -> Value {
    json!(t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect::<Vec<_>>())
}

/// A value that fits a JSON Schema: its example, enum, format or type (A8 routes).
fn from_schema(sc: &Value, depth: usize) -> Value {
    if let Some(e) = sc.get("example").or(sc.get("default")).or(sc["enum"].get(0)) { return e.clone(); }
    if let Some(x) = sc["allOf"].as_array() { let mut o = json!({}); for p in x { if let Value::Object(m) = from_schema(p, depth) { for (k, v) in m { o[k] = v; } } } return o; }
    if let Some(x) = sc["oneOf"].get(0).or(sc["anyOf"].get(0)) { return from_schema(x, depth); }
    let ty = sc["type"].as_str().unwrap_or(if sc.get("properties").is_some() { "object" } else { "string" });
    match ty {
        "object" if depth < 5 => Value::Object(sc["properties"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| (k, from_schema(&v, depth + 1))).collect()),
        "array" if depth < 5 => json!([from_schema(&sc["items"], depth + 1)]),
        "integer" => sc.get("minimum").cloned().unwrap_or(json!(1)),
        "number" => json!(1.5),
        "boolean" => json!(true),
        _ => json!(match s(sc, "format").as_str() { "email" => "user@example.com", "date" => "2026-01-01", "date-time" => "2026-01-01T12:00:00Z", _ => "sample" }),
    }
}

/// A sample body for Try it, from the route's schema, or the fields it receives and the plugin's data.
fn sample(route: &Value, plugin: &Value) -> String {
    if let Some(sc) = plugin["spec"]["schemas"].get(s(&route["receive"], "schema")) {
        return serde_json::to_string_pretty(&from_schema(sc, 0)).unwrap_or_default();
    }
    let cpf = plugin["data"]["people"][0]["cpf"].as_str().unwrap_or("123.456.789-09").to_string();
    let selfie = plugin["data"]["selfies"][0]["base64"].as_str().unwrap_or("c2VsZmll").to_string();
    let mut body = json!({});
    for f in route["receive"]["body"].as_array().cloned().unwrap_or_default() {
        let path: Vec<&str> = f.as_str().unwrap_or("").split('.').collect();
        let leaf = *path.last().unwrap_or(&"");
        let val = if leaf.contains("cpf") { json!(cpf) } else if leaf.contains("face") || leaf.contains("selfie") { json!(selfie) } else { json!("sample") };
        let mut cur = &mut body;
        for (i, k) in path.iter().enumerate() {
            if i + 1 == path.len() { cur[*k] = val.clone(); } else { if !cur[*k].is_object() { cur[*k] = json!({}); } cur = &mut cur[*k]; }
        }
    }
    if s(&route["receive"], "auth") == "bearer" || route["receive"]["body"].to_string().contains("cpf") {
        body["consentimento"] = json!("9f2c…be01");
    }
    serde_json::to_string_pretty(&body).unwrap_or_default()
}

#[component]
pub fn Routes() -> impl IntoView {
    let p = use_params_map();
    let qm = use_query_map();
    let live = expect_context::<api::Live>();
    let service = Memo::new(move |_| p.get().get("service").unwrap_or_default());
    let plugin = RwSignal::new(Value::Null);
    let spec_busy = RwSignal::new(false);
    let svc_list = super::services();
    let svc_urls = Memo::new(move |_| svc_list.get().into_iter().find(|x| s(x, "name") == service.get()).map(|x| x["urls"].clone()).unwrap_or(Value::Null));
    let sel = RwSignal::new(String::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let step = RwSignal::new("decide".to_string());
    let err = RwSignal::new(String::new());
    let conflict = RwSignal::new(false);
    let yaml_mode = RwSignal::new(false);
    let yaml_text = RwSignal::new(String::new());
    let yaml_errs = RwSignal::new(Vec::<Value>::new());
    let trial = RwSignal::new(Value::Null);
    let try_body = RwSignal::new(String::new());
    let counts = RwSignal::new(Value::Null);
    let add_menu = RwSignal::new(false);
    let pending = RwSignal::new(None::<Value>);
    let leave = RwSignal::new(false);
    let load = move |pick: Option<String>| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            let Ok(v) = api::get(&format!("/plugins/{}", enc(&sv))).await else { return };
            let routes = v["plugin"]["routes"].as_array().cloned().unwrap_or_default();
            let want = pick.unwrap_or(sel.get_untracked());
            let r = routes.iter().find(|r| s(r, "route") == want).or(routes.iter().find(|r| r["token"] != true)).or(routes.first()).cloned();
            plugin.set(v.clone());
            if !yaml_mode.get_untracked() { yaml_text.set(s(&v, "yaml")); }
            if sel.get_untracked() != "\u{0}new" && (draft.get_untracked() == saved.get_untracked() || saved.get_untracked().is_null()) {
                if let Some(r) = r { sel.set(s(&r, "route")); saved.set(r.clone()); draft.set(r.clone()); try_body.set(sample(&r, &v["plugin"])); }
            }
            if let Ok(b) = api::get(&format!("/apis/calls?service={}&since=1d&limit=500", enc(&sv))).await {
                let mut m = serde_json::Map::new();
                for c in b["calls"].as_array().cloned().unwrap_or_default() { let k = s(&c, "route"); let n = m.get(&k).and_then(|x| x.as_i64()).unwrap_or(0); m.insert(k, json!(n + 1)); }
                counts.set(Value::Object(m));
            }
        });
    };
    Effect::new(move |_| {
        service.track();
        sel.set(String::new()); saved.set(Value::Null); draft.set(Value::Null);
        let q = qm.get_untracked();
        if let Some(d) = q.get("draft") {
            leptos::task::spawn_local(async move {
                if let Ok(v) = api::send("POST", &format!("/apis/unmatched/{}/route", enc(&d)), json!({})).await {
                    let r = v["route"].clone();
                    sel.set("\u{0}new".into()); saved.set(Value::Null); draft.set(r);
                    load(None);
                }
            });
        } else {
            load(q.get("route"));
        }
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        if prev.is_some() && s(&live.last.get_untracked(), "topic") == "apis.plugin" { load(None); }
        t
    });
    let dirty = Memo::new(move |_| {
        let (a, b) = (saved.get(), draft.get());
        if b.is_null() { return 0; }
        if a.is_null() { return 1; }
        ["/route", "/receive", "/let", "/keep", "/wait", "/decide", "/respond", "/send", "/fail", "/billable"].iter().filter(|p| a.pointer(p) != b.pointer(p)).count()
    });
    let set = move |path: &'static str, v: Value| draft.update(|d| {
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        let mut cur = d;
        for (i, k) in parts.iter().enumerate() {
            if i + 1 == parts.len() { if v.is_null() { if let Some(o) = cur.as_object_mut() { o.remove(*k); } } else { cur[*k] = v.clone(); } }
            else { if !cur[*k].is_object() { cur[*k] = json!({}); } cur = &mut cur[*k]; }
        }
    });
    let save = move |keep_both: bool| {
        let (sv, d, old) = (service.get_untracked(), draft.get_untracked(), saved.get_untracked());
        let route = s(&d, "route");
        let (m, path) = route.split_once(' ').map(|(a, b)| (a.to_string(), b.trim().to_string())).unwrap_or(("GET".into(), "/".into()));
        let mut body = json!({"method": m, "path": path, "steps": d});
        if !old.is_null() && s(&old, "route") != route { body["old_route"] = json!(s(&old, "route")); }
        if !keep_both { body["version"] = plugin.get_untracked()["version"].clone(); }
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}/routes", enc(&sv)), body).await {
                Ok(_) => { err.set(String::new()); conflict.set(false); saved.set(draft.get_untracked()); sel.set(route.clone()); ui::toast("Saved in the plugin file"); load(Some(route)); }
                Err(e) if e.contains("changed elsewhere") => conflict.set(true),
                Err(e) => err.set(e),
            }
        });
    };
    let save_yaml = move || {
        let (sv, t, v) = (service.get_untracked(), yaml_text.get_untracked(), plugin.get_untracked()["version"].clone());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/plugins/{}", enc(&sv)), json!({"yaml": t, "version": v})).await {
                Ok(_) => { yaml_mode.set(false); saved.set(Value::Null); ui::toast("YAML saved"); load(None); }
                Err(e) => err.set(e),
            }
        });
    };
    let check_yaml = move |t: String| {
        let sv = service.get_untracked();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::send("POST", &format!("/plugins/{}/validate", enc(&sv)), json!({"yaml": t})).await {
                yaml_errs.set(v["errors"].as_array().cloned().unwrap_or_default());
            }
        });
    };
    let run_try = move |_| {
        let (sv, d) = (service.get_untracked(), draft.get_untracked());
        let route = s(&d, "route");
        let (m, path) = route.split_once(' ').map(|(a, b)| (a.to_string(), b.trim().to_string())).unwrap_or(("GET".into(), "/".into()));
        let body: Value = serde_json::from_str(&try_body.get_untracked()).unwrap_or(json!({}));
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/plugins/{}/try", enc(&sv)), json!({"method": m, "path": path, "steps": d, "request": {"headers": {"authorization": "Bearer try-it"}, "body": body}})).await {
                Ok(v) => trial.set(v),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let new_route = move |_| {
        let first = plugin.get_untracked()["plugin"]["answers"][0]["name"].as_str().unwrap_or("ok").to_string();
        sel.set("\u{0}new".into()); saved.set(Value::Null);
        draft.set(json!({"route": "GET /new", "receive": {}, "decide": [{"answer": first}], "respond": {}}));
        step.set("receive".into());
    };
    view! {
        <div class="page">
            {move || view! { <PluginHead service=service.get() active="routes" right=move || view! {
                {move || { let d = dirty.get(); (d > 0).then(|| view! { <span class="small attn">{format!("{d} unsaved change{}", if d == 1 { "" } else { "s" })}</span>
                    <button class="btn" on:click=move |_| { if saved.get_untracked().is_null() { draft.set(Value::Null); load(None); } else { draft.set(saved.get_untracked()); } err.set(String::new()); }>"Discard"</button> }) }}
                {move || if yaml_mode.get() {
                    view! { <button class="btn primary" disabled=move || !yaml_errs.get().is_empty() on:click=move |_| save_yaml()>"Save YAML"</button> }.into_any()
                } else {
                    view! { <button class="btn primary" disabled=move || dirty.get() == 0 on:click=move |_| save(false)>"Save"</button> }.into_any()
                }}
            }/> }}
            {move || conflict.get().then(|| view! {
                <div class="strip">{format!("{} changed elsewhere while you edited (by an agent or by hand). Your change is in another place of the file; both can be kept.", s(&plugin.get(), "file"))}
                    <button class="link" on:click=move |_| { leptos::task::spawn_local(async move { load(None); save(true); }); }>"Keep both"</button>
                    <button class="link" on:click=move |_| { conflict.set(false); draft.set(saved.get_untracked()); load(None); }>"Load the file"</button></div>
            })}
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">{move || if plugin.get()["plugin"]["spec"].is_null() { "Routes" } else { "Routes · from the spec" }}</div>
                    {move || plugin.get()["plugin"]["routes"].as_array().cloned().unwrap_or_default().into_iter().map(|r| {
                        let key = s(&r, "route");
                        let (k1, k2) = (key.clone(), key.clone());
                        let (m, path) = key.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                        let n = counts.get()[&key].as_i64().unwrap_or(0);
                        view! { <div class="rrow" class:on=move || sel.get() == k1 on:click=move |_| {
                                if dirty.get_untracked() > 0 { pending.set(Some(r.clone())); leave.set(true); return; }
                                sel.set(k2.clone()); saved.set(r.clone()); draft.set(r.clone()); trial.set(Value::Null); try_body.set(sample(&r, &plugin.get_untracked()["plugin"]));
                            }>
                            <span class="mono small cap" style="width:40px">{m}</span>
                            <div class="grow"><div class="mono small">{path}</div>{(n > 0 || r["token"] == true).then(|| view! { <div class="small cap">{if r["token"] == true { "token (client credentials)".to_string() } else { format!("{n} calls today") }}</div> })}</div></div> }
                    }).collect_view()}
                    <button class="link add" on:click=new_route>"+ Add route"</button>
                    {move || { let sp = plugin.get()["plugin"]["spec"].clone(); (!sp.is_null()).then(|| {
                        let sv = service.get();
                        let file = s(&sp, "openapi");
                        let url = format!("{}/{}", s(&svc_urls.get(), "local"), file);
                        view! { <div style="padding:16px 24px;border-top:1px solid var(--rule);margin-top:8px">
                            <div class="s12" style="font-weight:500">{format!("{file}  ·  {} {}", if s(&sp, "version").starts_with('2') { "Swagger" } else { "OpenAPI" }, s(&sp, "version"))}</div>
                            <div class="small cap" style="margin:2px 0 10px">{format!("{}{} schema{}{}", if s(&sp, "api_version").is_empty() { String::new() } else { format!("v{}  ·  ", s(&sp, "api_version")) }, sp["schemas"].as_object().map(|x| x.len()).unwrap_or(0), if sp["schemas"].as_object().map(|x| x.len()).unwrap_or(0) == 1 { "" } else { "s" }, if sp["validate"] == false { "  ·  bodies not checked" } else { "  ·  bodies checked" })}</div>
                            <button class="link" style="display:block;font-size:13px" disabled=move || spec_busy.get() on:click=move |_| {
                                let sv = sv.clone();
                                spec_busy.set(true);
                                leptos::task::spawn_local(async move {
                                    match api::send("POST", &format!("/plugins/{}/spec", enc(&sv)), json!({})).await {
                                        Ok(v) => { let (a, r) = (v["added"].as_array().map(|x| x.len()).unwrap_or(0), v["notInSpec"].as_array().map(|x| x.len()).unwrap_or(0)); ui::toast(if a + r == 0 { "The spec has the same routes".to_string() } else { format!("{a} route(s) added; {r} not in the spec, kept") }); load(None); }
                                        Err(e) => ui::toast_err(e),
                                    }
                                    spec_busy.set(false);
                                });
                            }>"Re-import the spec"</button>
                            <a class="link" style="display:block;font-size:13px;margin-top:6px" target="_blank" href=url>"Open the served spec ›"</a></div> }
                    }) }}
                    <div class="small cap" style="padding:8px 24px">{move || if plugin.get()["plugin"]["spec"].is_null() { "A route is what the plugin does with one kind of request." } else { "Routes made from a spec are normal routes: the spec gave the path, the schema and the answers." }}</div>
                </div>
                <div class="pane pad form">
                    {move || {
                        let d = draft.get();
                        let pl = plugin.get();
                        if pl.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        if d.is_null() {
                            return view! { <div class="callout"><div class="t">"A new plugin with no routes"</div>
                                <div class="t2">"Every call is answered 404 “no route” and listed as Not answered on the Timeline; create routes from there in one click."</div>
                                <a class="link" href="/apis/unmatched">"Not answered ›"</a></div> }.into_any();
                        }
                        let answers: Vec<String> = pl["plugin"]["answers"].as_array().cloned().unwrap_or_default().iter().map(|a| s(a, "name")).collect();
                        let n_today = counts.get()[&s(&d, "route")].as_i64().unwrap_or(0);
                        let token = d["token"] == true;
                        let card = move |key: &'static str, n: usize, title: &'static str, summary: String, extra: AnyView| {
                            view! {
                                <div class="stepcard" class:on=move || step.get() == key on:click=move |_| step.set(key.into())>
                                    <div class="row"><span class="stepn" class:on=move || step.get() == key>{n}</span><span class="caps">{title}</span></div>
                                    <div class="stepsum">{summary}</div>
                                    {extra}
                                </div>
                            }
                        };
                        let rcv = d["receive"].clone();
                        let auth = s(&rcv, "auth");
                        let rcv_sum = if token { "OAuth2 client credentials: issues a token (Auth and limits sets the lifetime)".to_string() } else {
                            format!("{}{}", match auth.as_str() { "bearer" => "Bearer token from the token route", "basic" => "HTTP Basic", _ => "No auth" }, if !s(&rcv, "schema").is_empty() { format!("  ·  body checked against {}{}", s(&rcv, "schema"), if s(&rcv, "invalid").is_empty() { String::new() } else { format!(", else “{}”", s(&rcv, "invalid")) }) } else if rcv["body"].as_array().map(|a| a.is_empty()).unwrap_or(true) { String::new() } else { format!("  ·  body: {}", list_text(&rcv["body"])) }) };
                        let lets = d["let"].as_object().cloned().unwrap_or_default();
                        let keep = d["keep"].clone();
                        let wait = d["wait"].clone();
                        let decide = d["decide"].as_array().cloned().unwrap_or_default();
                        let send = d["send"].as_array().cloned().unwrap_or_default();
                        let ans2 = answers.clone();
                        let a0_default = answers.first().cloned().unwrap_or("ok".into());
                        let n_answers = answers.len();
                        let mut n = 0usize;
                        let mut next = || { n += 1; n };
                        view! {
                            <div class="row"><input class="input mono" style="width:520px;font-size:15px;font-weight:500" prop:value=s(&d, "route") on:change=move |e| set("/route", json!(event_target_value(&e).trim().to_string()))/></div>
                            <div class="small attn" style="margin:6px 0 16px">{format!("{n_today} calls today{}", if d["billable"] == true { "  ·  billable" } else { "" })}
                                <label class="check" style="margin-left:14px"><input type="checkbox" prop:checked=d["billable"] == true on:change=move |e| set("/billable", if event_target_checked(&e) { json!(true) } else { Value::Null })/>"billable"</label></div>
                            {card("receive", next(), "Receive", rcv_sum, if token { ().into_any() } else { view! {
                                <Show when=move || step.get() == "receive">
                                    <div class="stepedit">
                                        <div class="row"><span style="width:110px">"Auth"</span>
                                            <select class="select" on:change=move |e| set("/receive/auth", json!(event_target_value(&e)))>
                                                {[("none", "None"), ("bearer", "Bearer token (from the token route)"), ("basic", "HTTP Basic")].into_iter().map(|(v, l)| { let on = s(&draft.get_untracked()["receive"], "auth") == v || (v == "none" && s(&draft.get_untracked()["receive"], "auth").is_empty()); view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select></div>
                                        <div class="row" style="margin-top:8px"><span style="width:110px">"Required"</span><input class="input mono grow" placeholder="cpf, validacao.biometria_face" prop:value=list_text(&draft.get_untracked()["receive"]["body"]) on:change=move |e| set("/receive/body", parse_list(&event_target_value(&e)))/></div>
                                        <div class="small cap" style="margin-top:4px">"A missing field is answered 400, like the provider."</div>
                                    </div></Show> }.into_any() })}
                            {(!lets.is_empty()).then(|| card("let", next(), "Let", lets.iter().map(|(k, v)| format!("{k} = {}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n"), view! {
                                <Show when=move || step.get() == "let">
                                    <div class="stepedit">{draft.get_untracked()["let"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| { let k2 = k.clone(); view! {
                                        <div class="row" style="margin-bottom:6px"><span class="mono" style="width:110px">{k.clone()}</span>
                                            <input class="input mono grow" prop:value=v.as_str().unwrap_or("").to_string() on:change=move |e| { let k = k2.clone(); let v = event_target_value(&e); draft.update(|d| d["let"][&k] = json!(v)); }/></div> } }).collect_view()}</div>
                                </Show> }.into_any()))}
                            {(!keep.is_null()).then(|| card("keep", next(), "Keep", format!("Save to state “{}” by {}, with the time", s(&keep, "state"), s(&keep, "key")), view! {
                                <Show when=move || step.get() == "keep">
                                    <div class="stepedit row"><span style="width:60px">"State"</span><input class="input mono" style="width:160px" prop:value=s(&draft.get_untracked()["keep"], "state") on:change=move |e| set("/keep/state", json!(event_target_value(&e)))/>
                                        <span style="width:40px;margin-left:10px">"Key"</span><input class="input mono grow" prop:value=s(&draft.get_untracked()["keep"], "key") on:change=move |e| set("/keep/key", json!(event_target_value(&e)))/>
                                        <button class="link plain" on:click=move |_| set("/keep", Value::Null)>"Remove"</button></div>
                                </Show> }.into_any()))}
                            {(!wait.is_null()).then(|| card("wait", next(), "Wait", format!("Answer “{}” {} times per {}, then “{}”", s(&wait, "answer"), api::n(&wait, "times"), s(&wait, "key"), s(&wait, "then")), view! {
                                <Show when=move || step.get() == "wait">
                                    <div class="stepedit row" style="flex-wrap:wrap;gap:8px">"Answer"
                                        <input class="input mono" style="width:120px" prop:value=s(&draft.get_untracked()["wait"], "answer") on:change=move |e| set("/wait/answer", json!(event_target_value(&e)))/>
                                        <input class="input mono" style="width:50px" prop:value=api::n(&draft.get_untracked()["wait"], "times").to_string() on:change=move |e| set("/wait/times", json!(event_target_value(&e).parse::<u64>().unwrap_or(3)))/>"times per"
                                        <input class="input mono" style="width:100px" prop:value=s(&draft.get_untracked()["wait"], "key") on:change=move |e| set("/wait/key", json!(event_target_value(&e)))/>", then"
                                        <input class="input mono" style="width:100px" prop:value=s(&draft.get_untracked()["wait"], "then") on:change=move |e| set("/wait/then", json!(event_target_value(&e)))/>
                                        <button class="link plain" on:click=move |_| set("/wait", Value::Null)>"Remove"</button></div>
                                </Show> }.into_any()))}
                            {(!token).then(|| card("decide", next(), "Decide", if decide.is_empty() { "nothing decided yet".into() } else { format!("First match wins:\n{}", decide.iter().map(|x| format!("  {:<34} → {}", if x["when"].is_null() { "otherwise".to_string() } else { s(x, "when") }, s(x, "answer").replace('_', " "))).collect::<Vec<_>>().join("\n")) }, view! {
                                <Show when=move || step.get() == "decide">
                                    <div class="stepedit">
                                        {draft.get_untracked()["decide"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, x)| { let ans = ans2.clone(); view! {
                                            <div class="row" style="margin-bottom:6px">
                                                <input class="input mono grow" placeholder="otherwise" prop:value=s(&x, "when") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| { if v.trim().is_empty() { if let Some(o) = d["decide"][i].as_object_mut() { o.remove("when"); } } else { d["decide"][i]["when"] = json!(v); } }); }/>
                                                <span>"→"</span>
                                                <select class="select" on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["decide"][i]["answer"] = json!(v)); }>
                                                    {ans.iter().chain(["reset", "empty", "timeout", "401", "403", "500", "503", "html"].iter().map(|x| x.to_string()).collect::<Vec<_>>().iter()).map(|a| { let on = *a == s(&x, "answer"); view! { <option value=a.clone() selected=on>{a.replace('_', " ")}</option> } }).collect_view()}</select>
                                                <button class="link plain" on:click=move |_| draft.update(|d| { if let Some(a) = d["decide"].as_array_mut() { a.remove(i); } })>"×"</button></div> } }).collect_view()}
                                        <button class="link" on:click={ let a0 = a0_default.clone(); move |_| { let a0 = a0.clone(); draft.update(|d| { if !d["decide"].is_array() { d["decide"] = json!([]); } let arr = d["decide"].as_array_mut().unwrap(); let at = arr.len().saturating_sub(1); arr.insert(at, json!({"when": "body.x == 'y'", "answer": a0})); }) } }>"+ A rule (above “otherwise”)"</button>
                                        <div class="small cap" style="margin-top:6px">"Expressions are CEL-like: body.cpf in data.irregular_cpfs, selfie.tag != person.tag, has(body.x)."</div>
                                    </div>
                                </Show> }.into_any()))}
                            {card("respond", next(), "Respond", format!("With the named answer{}", { let l = s(&d["respond"], "latency"); if l.is_empty() { "  ·  latency from Auth and limits".to_string() } else { format!("  ·  latency {l}") } }), view! {
                                <a class="link small" style="display:block;margin-top:4px" href=format!("/apis/{}/answers", enc(&service.get_untracked()))>{format!("the answers live in Answers {} ›", n_answers)}</a>
                                <Show when=move || step.get() == "respond">
                                    <div class="stepedit row"><span style="width:110px">"Latency"</span><input class="input mono" style="width:180px" placeholder="300ms..600ms" prop:value=s(&draft.get_untracked()["respond"], "latency") on:change=move |e| set("/respond/latency", json!(event_target_value(&e)))/>
                                        <span style="width:90px;margin-left:12px">"Fail always"</span>
                                        <select class="select" on:change=move |e| { let v = event_target_value(&e); set("/fail", if v.is_empty() { Value::Null } else { json!(v) }); }>
                                            {[("", "No"), ("reset", "Connection reset"), ("empty", "Empty reply"), ("timeout", "Timeout"), ("401", "401"), ("403", "403"), ("500", "500"), ("503", "503"), ("html", "HTML with 200")].into_iter().map(|(v, l)| { let on = s(&draft.get_untracked(), "fail") == v; view! { <option value=v selected=on>{l}</option> } }).collect_view()}</select></div>
                                </Show> }.into_any())}
                            {card("send", next(), "Send", if send.is_empty() { "Nothing: no webhook after responding".into() } else { send.iter().map(|x| format!("{} {}{}", s(x, "method").to_uppercase(), s(x, "url"), if s(x, "delay").is_empty() { String::new() } else { format!(" after {}", s(x, "delay")) })).collect::<Vec<_>>().join("\n") }, view! {
                                <Show when=move || step.get() == "send">
                                    <div class="stepedit">{draft.get_untracked()["send"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, x)| view! {
                                        <div class="row" style="margin-bottom:6px"><input class="input mono grow" placeholder="https://app/hooks or {{ body.callback_url }}" prop:value=s(&x, "url") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["send"][i]["url"] = json!(v)); }/>
                                            <input class="input mono" style="width:70px" placeholder="delay" prop:value=s(&x, "delay") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["send"][i]["delay"] = json!(v)); }/>
                                            <button class="link plain" on:click=move |_| draft.update(|d| { if let Some(a) = d["send"].as_array_mut() { a.remove(i); } })>"×"</button></div>
                                        <textarea class="yaml" style="min-height:70px;margin-bottom:8px" prop:value=s(&x, "body") on:change=move |e| { let v = event_target_value(&e); draft.update(|d| d["send"][i]["body"] = json!(v)); }></textarea> }).collect_view()}</div>
                                </Show>
                                <button class="link small" on:click=move |e| { e.stop_propagation(); step.set("send".into()); draft.update(|d| { if !d["send"].is_array() { d["send"] = json!([]); } d["send"].as_array_mut().unwrap().push(json!({"url": "{{ body.callback_url }}", "body": "{\"event\": \"done\"}", "delay": "1s"})); }) }>"+ Send a webhook after responding"</button> }.into_any())}
                            <div class="row" style="margin-top:10px;gap:16px">
                                <span class="anchor"><button class="link" on:click=move |_| add_menu.set(true)>"+ Add step"</button>
                                    <Pop open=add_menu>
                                        <div class="mi" on:click=move |_| { set("/keep", json!({"state": "items", "key": "body.id"})); step.set("keep".into()); }>"Keep in state"</div>
                                        <div class="mi" on:click=move |_| { draft.update(|d| { if !d["let"].is_object() { d["let"] = json!({}); } d["let"]["value"] = json!("lookup('table', 'field', body.x)"); }); step.set("let".into()); }>"Let (a named value)"</div>
                                        <div class="mi" on:click=move |_| { step.set("decide".into()); }>"Decide (first match wins)"</div>
                                        <div class="mi" on:click=move |_| { let a = draft.get_untracked()["decide"][0]["answer"].as_str().unwrap_or("ok").to_string(); set("/wait", json!({"key": "path.id", "answer": a, "times": 3, "then": a})); step.set("wait".into()); }>"Wait (answer N times, then)"</div>
                                        <div class="mi" on:click=move |_| { draft.update(|d| { if !d["send"].is_array() { d["send"] = json!([]); } d["send"].as_array_mut().unwrap().push(json!({"url": "{{ body.callback_url }}", "body": "{}", "delay": "1s"})); }); step.set("send".into()); }>"Send a webhook"</div>
                                    </Pop></span>
                                <span class="small cap">"Try it sends a sample request through these steps without the app"</span>
                            </div>
                            <div class="trybox">
                                <div class="row"><span class="caps grow">"Try it"</span><button class="btn small" on:click=run_try>"Run"</button></div>
                                <textarea class="yaml" style="min-height:90px;margin-top:8px" prop:value=move || try_body.get() on:input=move |e| try_body.set(event_target_value(&e))></textarea>
                                {move || { let t = trial.get(); (!t.is_null()).then(|| view! {
                                    <div class="small" style="margin-top:8px">{t["steps"].as_array().cloned().unwrap_or_default().iter().enumerate().map(|(i, x)| format!("{} {} {}", i + 1, s(x, "step"), if x["ok"] == true { "ok" } else { "✗" })).collect::<Vec<_>>().join("  ·  ")}</div>
                                    {t["steps"].as_array().cloned().unwrap_or_default().into_iter().map(|x| view! { <div class="small" class:danger=x["ok"] != true class:t2=x["ok"] == true>{format!("{}: {}", s(&x, "step"), s(&x, "text"))}</div> }).collect_view()}
                                    <div class="small" style="margin-top:6px">{format!("→ {} {}  ·  {}  ·  {}", api::n(&t, "status"), s(&t, "answer"), s(&t, "sentence"), s(&t, "note"))}</div>
                                    <pre class="yaml envyaml" style="max-height:160px;overflow:auto">{serde_json::to_string_pretty(&t["body"]).unwrap_or_default()}</pre>
                                }) }}
                            </div>
                            {move || (!err.get().is_empty()).then(|| view! { <div class="callout danger-l"><div class="t danger">{err.get()}</div><div class="t2">"Save stays off until it is fixed; the YAML shows the same error on that line."</div></div> })}
                        }.into_any()
                    }}
                </div>
                <div class="side yaml" style="width:430px">
                    <div class="row"><span class="caps grow">{move || s(&plugin.get(), "file")}</span><span class="small ok">"live"</span>
                        <button class="link" style="margin-left:10px" on:click=move |_| { let on = !yaml_mode.get_untracked(); yaml_mode.set(on); if on { yaml_text.set(s(&plugin.get_untracked(), "yaml")); } }>{move || if yaml_mode.get() { "Back to the steps" } else { "Edit the YAML" }}</button></div>
                    {move || if yaml_mode.get() {
                        view! {
                            <textarea class="yaml" style="min-height:70vh;margin-top:10px" prop:value=move || yaml_text.get() on:input=move |e| { let v = event_target_value(&e); yaml_text.set(v.clone()); check_yaml(v); }></textarea>
                            {move || yaml_errs.get().into_iter().map(|e| view! { <div class="small danger">{format!("{}{}", e["line"].as_i64().map(|l| format!("line {l}: ")).unwrap_or_default(), s(&e, "error"))}</div> }).collect_view()}
                            <div class="small cap">"Typing in the YAML is checked as you type; the steps follow the file after Save YAML."</div>
                        }.into_any()
                    } else {
                        let route = s(&draft.get(), "route");
                        let text = s(&plugin.get(), "yaml");
                        let mut inside = false;
                        view! {
                            <pre class="yaml envyaml" style="margin-top:10px">{text.lines().map(|l| {
                                let t = l.trim_start();
                                if t.starts_with("- route:") { inside = t.trim_start_matches("- route:").trim() == route; }
                                else if !l.starts_with(' ') && !l.is_empty() { inside = false; }
                                let on = inside;
                                view! { <div class="yl" class:shade=on>{l.to_string()}</div> }
                            }).collect_view()}</pre>
                            <div class="small cap">"The selected route is shaded. Expressions are CEL-like; a plugin can't reach the network or disk unless it declares a webhook."</div>
                        }.into_any()
                    }}
                </div>
            </div>
            <ui::Confirm open=leave question=Signal::derive(move || format!("Discard {} change(s) to this route?", dirty.get()))
                detail=Signal::derive(|| "The route keeps running as it was saved.".to_string()) action="Discard"
                on_yes=move |_| { if let Some(r) = pending.get_untracked() { sel.set(s(&r, "route")); saved.set(r.clone()); draft.set(r.clone()); trial.set(Value::Null); try_body.set(sample(&r, &plugin.get_untracked()["plugin"])); } }/>
        </div>
    }
}
