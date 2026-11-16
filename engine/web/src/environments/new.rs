//! N2 · New environment: a name, where it starts from (a copy, a template with ${…} values, empty,
//! a folder), what a copy takes, addresses, lifetime and callbacks; the right side shows what will be created. Create calls
//! create_environment, the same call a pipeline makes.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{all, label};
use crate::{
    api::{self, s},
    ui::{self, Toggle},
};

const LARGE_STATE: i64 = 200 << 20;

#[component]
pub fn New() -> impl IntoView {
    let nav = use_navigate();
    let q = use_query_map();
    let env = expect_context::<RwSignal<Value>>();
    let list = all();
    let name = RwSignal::new(String::new());
    // copy | empty | file
    let start = RwSignal::new("copy".to_string());
    let from = RwSignal::new(String::new());
    let folder = RwSignal::new(String::new());
    let copy = RwSignal::new(vec!["services", "scenarios", "inboxes", "secrets"]);
    let expires = RwSignal::new("24h".to_string());
    let callbacks = RwSignal::new(String::new());
    let templates = RwSignal::new(Vec::<Value>::new());
    let tpl = RwSignal::new(String::new());
    let vars = RwSignal::new(serde_json::Map::<String, Value>::new());
    let base_domain = RwSignal::new(String::new());
    let public_url = RwSignal::new(String::new());
    leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/environments/templates").await {
            let t = v["templates"].as_array().cloned().unwrap_or_default();
            if let Some(f) = t.first() { tpl.set(s(f, "name")); }
            templates.set(t);
        }
    });
    let template = Memo::new(move |_| templates.get().into_iter().find(|t| s(t, "name") == tpl.get()).unwrap_or(Value::Null));
    let creating = RwSignal::new(false);
    let taken = RwSignal::new(String::new());
    let err = RwSignal::new(String::new());
    let problems = RwSignal::new(Vec::<Value>::new());
    let made = RwSignal::new(String::new());

    Effect::new(move |_| {
        let f = q.get().get("from").unwrap_or_else(|| s(&env.get(), "env"));
        if from.get_untracked().is_empty() && !f.is_empty() {
            from.set(f);
        }
    });
    let source = Memo::new(move |_| list.get().into_iter().find(|e| s(e, "name") == from.get()).unwrap_or(Value::Null));
    // The source's callback target is the starting point of the new one.
    Effect::new(move |_| {
        let src = source.get();
        if callbacks.get_untracked().is_empty() && !src.is_null() {
            callbacks.set(s(&src["callbacks"], "default"));
        }
    });
    let valid = move || {
        let n = name.get();
        !n.is_empty() && n.len() <= 40 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && !n.starts_with('-') && !n.ends_with('-')
    };
    let exists = move || list.get().iter().any(|e| s(e, "name") == name.get());
    let has = move |k: &str| copy.get().contains(&k);
    let flip = move |k: &'static str, on: bool| copy.update(|c| { c.retain(|x| *x != k); if on { c.push(k) } });
    let nm = move || if name.get().is_empty() { "<name>".to_string() } else { name.get() };
    let base = move || {
        if !base_domain.get().trim().is_empty() { return base_domain.get().trim().to_string(); }
        let t = s(&template.get(), "baseDomain");
        if start.get() == "template" && !t.is_empty() { t.replace("${name}", &nm()) } else { format!("{}.stuntman.local", nm()) }
    };
    // Where browsers will open it: the URL typed, else the server's public address + /e/<name>.
    let browsers = move || {
        let u = public_url.get().trim().trim_end_matches('/').to_string();
        if !u.is_empty() { return u; }
        let p = s(&env.get(), "publicUrl");
        match p.rsplit_once("/e/") { Some((root, _)) if env.get()["shared"] == true => format!("{root}/e/{}", nm()), _ => format!("http://{}/e/{}", s(&env.get(), "host"), nm()) }
    };
    let size = move || {
        let src = source.get();
        if start.get() != "copy" || src.is_null() { return "a few KB".to_string(); }
        let files = if has("services") || has("inboxes") || has("secrets") { api::n(&src["sizes"], "files") } else { 0 };
        if has("state") { format!("about {} (with state)", api::kb(files + api::n(&src["sizes"], "state"))) } else { format!("about {} (seeds only)", api::kb(files.max(1))) }
    };
    let expiry_text = move || match expires.get().as_str() {
        "never" => "never (kept)".to_string(),
        "pr_closed" => "when the pull request closes".into(),
        d => {
            let ms = match d { "1h" => 3_600_000, "72h" => 3 * 86_400_000, "7d" => 7 * 86_400_000, _ => 86_400_000 };
            format!("{} {}", super::day(api::now() as i64 + ms), api::hm(api::now() as i64 + ms))
        }
    };
    let n_cancel = nav.clone();
    let n_done = nav.clone();
    let create = move |_| {
        let mut body = json!({"name": name.get_untracked(), "expires": expires.get_untracked()});
        match start.get_untracked().as_str() {
            "copy" => { body["from"] = json!(from.get_untracked()); body["copy"] = json!(copy.get_untracked()); }
            "file" => body["from"] = json!(folder.get_untracked()),
            "template" => { body["from"] = json!(tpl.get_untracked()); body["vars"] = Value::Object(vars.get_untracked()); }
            _ => body["from"] = json!("empty"),
        }
        // A template or a folder brings its own callbacks; only a copy starts from the source's.
        if start.get_untracked() == "copy" && !callbacks.get_untracked().trim().is_empty() {
            body["callbacks"] = json!({"default": callbacks.get_untracked().trim()});
        }
        if !base_domain.get_untracked().trim().is_empty() { body["base_domain"] = json!(base_domain.get_untracked().trim()); }
        if !public_url.get_untracked().trim().is_empty() { body["public_url"] = json!(public_url.get_untracked().trim()); }
        creating.set(true);
        err.set(String::new());
        taken.set(String::new());
        let nav = n_done.clone();
        leptos::task::spawn_local(async move {
            let r = api::send("POST", "/environments", body).await;
            creating.set(false);
            match r {
                Ok(v) => {
                    let p = v["problems"].as_array().cloned().unwrap_or_default();
                    if p.is_empty() {
                        ui::toast(format!("{} created", label(&s(&v, "name"))));
                        nav(&format!("/environments?sel={}", s(&v, "name")), Default::default());
                    } else {
                        made.set(s(&v, "name"));
                        problems.set(p);
                    }
                }
                Err(e) if e.contains("already exists") => taken.set(e),
                Err(e) => err.set(e),
            }
        });
    };

    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <h1><a class="crumb" href="/environments">"Environments"</a><span class="crumb" style="margin:0 12px">"/"</span>"New environment"</h1>
                    <span class="grow"></span>
                    <button class="btn" on:click=move |_| n_cancel("/environments", Default::default())>"Cancel"</button>
                    <button class="btn primary" disabled=move || !valid() || exists() || creating.get() || (start.get() == "file" && folder.get().trim().is_empty()) || (start.get() == "template" && tpl.get().is_empty()) on:click=create>
                        {move || if creating.get() { "Creating…" } else { "Create" }}</button>
                </div>
            </div>
            <div class="body">
                <div class="pane pad form"><div style="max-width:780px">
                    <div class="row" style="margin-bottom:8px">
                        <span style="width:164px">"Name"</span>
                        <input class="input mono" style="width:240px" class:bad=move || !name.get().is_empty() && (!valid() || exists()) placeholder="pr-5462"
                            prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e).trim().to_lowercase())/>
                        <span class="small cap">"lowercase, numbers and dashes, 1 to 40"</span>
                    </div>
                    {move || exists().then(|| { let n = name.get(); view! {
                        <div class="callout danger-l"><div class="t danger">{format!("{n} already exists")}</div>
                            <div class="t2">"Pick another name, or open the existing one."</div>
                            <a class="link" href=format!("/environments?sel={n}")>"Open the existing one"</a></div> } })}
                    {move || (!taken.get().is_empty()).then(|| view! { <div class="err">{taken.get()}</div> })}

                    <div class="caps" style="margin:22px 0 10px">"Start from"</div>
                    <div class="choice" class:on=move || start.get() == "copy" on:click=move |_| start.set("copy".into())>
                        <span class="r"></span>
                        <div class="grow"><div class="row">"A copy of "
                            <select class="select small" on:click=|e| e.stop_propagation() on:change=move |e| { from.set(event_target_value(&e)); callbacks.set(String::new()); }>
                                {move || list.get().into_iter().map(|e| { let n = s(&e, "name"); let on = n == from.get(); view! { <option value=n.clone() selected=on>{label(&n)}</option> } }).collect_view()}
                            </select></div>
                            <div class="small cap">"services, rules, scenarios and seeds; no traffic history unless you copy the state"</div></div>
                    </div>
                    <div class="choice" class:on=move || start.get() == "template" on:click=move |_| start.set("template".into())>
                        <span class="r"></span>
                        <div class="grow"><div class="row">"A template "
                            {move || if templates.get().is_empty() {
                                view! { <span class="small cap">"none yet: put a folder in templates/ on this Stuntman"</span> }.into_any()
                            } else { view! {
                                <select class="select small" on:click=|e| e.stop_propagation() on:change=move |e| { tpl.set(event_target_value(&e)); vars.set(Default::default()); }>
                                    {move || templates.get().into_iter().map(|t| { let n = s(&t, "name"); let on = n == tpl.get(); view! { <option value=n.clone() selected=on>{n.clone()}</option> } }).collect_view()}
                                </select> }.into_any() }}</div>
                            <div class="small cap">"a folder in templates/ on this Stuntman; ${…} values are filled in when it is copied"</div></div>
                    </div>
                    <div class="choice" class:on=move || start.get() == "empty" on:click=move |_| start.set("empty".into())>
                        <span class="r"></span><div><div>"Empty"</div><div class="small cap">"add services from the Catalog"</div></div>
                    </div>
                    <div class="choice" class:on=move || start.get() == "file" on:click=move |_| start.set("file".into())>
                        <span class="r"></span><div class="grow"><div>"A folder"</div><div class="small cap">"a path on the server, such as a repository checkout"</div>
                            {move || (start.get() == "file").then(|| view! {
                                <input class="input mono" style="width:100%;margin-top:8px" placeholder="/path/to/repo/environments/pr-5462" prop:value=move || folder.get()
                                    on:click=|e| e.stop_propagation() on:input=move |e| folder.set(event_target_value(&e))/> })}</div>
                    </div>

                    {move || (start.get() == "template" && !template.get().is_null()).then(|| {
                        let t = template.get();
                        let vs = t["vars"].as_array().cloned().unwrap_or_default();
                        let files: std::collections::BTreeSet<String> = vs.iter().flat_map(|v| v["files"].as_array().cloned().unwrap_or_default()).filter_map(|f| f.as_str().map(String::from)).collect();
                        view! {
                            <div class="caps" style="margin:22px 0 6px">"Values for the template"</div>
                            <div class="small cap" style="margin-bottom:12px">{format!("{} uses {} value{} in {} file{}. Each one becomes the text you give here.", s(&t, "name"), vs.len(), if vs.len() == 1 { "" } else { "s" }, files.len(), if files.len() == 1 { "" } else { "s" })}</div>
                            {vs.into_iter().map(|v| {
                                let k = s(&v, "var");
                                let where_ = v["files"].as_array().cloned().unwrap_or_default().iter().filter_map(|f| f.as_str().map(String::from)).collect::<Vec<_>>().join(", ");
                                let auto = v["auto"] == true;
                                let k2 = k.clone();
                                view! {
                                    <div class="row" style="margin-bottom:10px">
                                        <span class="mono" style="width:164px">{format!("${{{k}}}")}</span>
                                        {if auto { view! { <span class="mono cap" style="width:340px">{move || nm()}</span> }.into_any() } else { view! {
                                            <input class="input mono" style="width:340px" placeholder="value" prop:value=move || vars.get().get(&k).and_then(|x| x.as_str().map(String::from)).unwrap_or_default()
                                                on:input=move |e| { let val = event_target_value(&e); vars.update(|m| { m.insert(k2.clone(), json!(val)); }) }/> }.into_any() }}
                                        <span class="small cap" style="margin-left:12px">{if auto { "always the environment's name".to_string() } else { where_ }}</span>
                                    </div>
                                }
                            }).collect_view()}
                        }
                    })}
                    {move || (start.get() == "copy").then(|| view! {
                        <div class="caps" style="margin:22px 0 10px">{move || format!("Copy from {}", label(&from.get()))}</div>
                        {[("services", "Services and their files", ""), ("state", "State (messages, deliveries, telemetry…)", "State copies can be large; most pipelines start from seeds."),
                          ("scenarios", "Scenarios", "none in this build yet"), ("inboxes", "Inboxes", ""), ("secrets", "Secrets (names and seed values)", "")].into_iter().map(|(k, l, hint)| view! {
                            <div class="row" style="margin-bottom:10px">
                                <Toggle on=Signal::derive(move || has(k)) set=move |v: bool| flip(k, v)/>
                                <span style="width:300px">{l}</span><span class="small cap">{hint}</span>
                            </div>
                        }).collect_view()}
                        {move || (has("state") && api::n(&source.get()["sizes"], "state") > LARGE_STATE).then(|| view! {
                            <div class="callout attn-l"><div class="t attn">{format!("State of {} is {}", label(&from.get()), api::kb(api::n(&source.get()["sizes"], "state")))}</div>
                                <div class="t2">"Copying takes about a minute and doubles the disk used. Seeds only is usually enough."</div></div> })}
                    })}

                    <div class="block" style="margin-top:12px;border-top:1px solid var(--rule)">
                        <div class="caps" style="margin-bottom:14px">"Addresses and lifetime"</div>
                        <div class="row" style="margin-bottom:12px"><span style="width:164px">"Base domain"</span>
                            <input class="input mono" style="width:340px" placeholder=move || { let t = s(&template.get(), "baseDomain"); if start.get() == "template" && !t.is_empty() { t.replace("${name}", &nm()) } else { format!("{}.stuntman.local", nm()) } } prop:value=move || base_domain.get() on:input=move |e| base_domain.set(event_target_value(&e).trim().to_lowercase())/>
                            <span class="small cap" style="margin-left:12px">"apps reach every service by name under it"</span></div>
                        <div class="row" style="margin-bottom:12px"><span style="width:164px">"Public URL"</span>
                            <input class="input mono" style="width:340px" placeholder=browsers prop:value=move || public_url.get() on:input=move |e| public_url.set(event_target_value(&e))/>
                            <span class="small cap" style="margin-left:12px">"where browsers open it; sign-in pages and links use it"</span></div>
                        <div class="row" style="margin-bottom:12px"><span style="width:164px">"Expires"</span>
                            <select class="select" on:change=move |e| expires.set(event_target_value(&e))>
                                {[("never", "Never (kept)"), ("1h", "in 1 hour"), ("24h", "in 24 hours"), ("72h", "in 72 hours"), ("7d", "in 7 days"), ("pr_closed", "When the pull request closes")].into_iter()
                                    .map(|(v, l)| view! { <option value=v selected=move || expires.get() == v>{l}</option> }).collect_view()}
                            </select></div>
                        <div class="row" style:display=move || if start.get() == "copy" { "" } else { "none" }><span style="width:164px">"Callbacks go to"</span>
                            <input class="input mono grow" placeholder="https://pr-5462.acme-api.preview.example.com" prop:value=move || callbacks.get() on:input=move |e| callbacks.set(event_target_value(&e))/></div>
                    </div>
                    {move || creating.get().then(|| view! {
                        <div class="callout"><div class="t">{format!("Creating {}…", name.get())}</div>
                            <div class="t2">"Services start as soon as their files are copied; the list fills in as each answers."</div></div> })}
                    {move || (!problems.get().is_empty()).then(|| { let m = made.get(); view! {
                        <div class="callout danger-l">
                            {problems.get().into_iter().map(|p| view! { <div class="t danger">{format!("{}{}", s(&p, "file"), if p["line"].is_null() { String::new() } else { format!(" line {}", p["line"]) })}</div>
                                <div class="small t2 mono">{s(&p, "error")}</div> }).collect_view()}
                            <div class="t2" style="margin-top:6px">"The environment is created; a service whose file is broken answers 503 until the file is fixed."</div>
                            <a class="link" href=format!("/environments/{m}")>"Open the environment ›"</a></div> } })}
                    {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                    <div class="small cap" style="margin-top:20px">"Pipelines do the same with one call: POST /api/environments, or create_environment over MCP."</div>
                </div>                </div>
                <div class="side envside">
                    <div class="caps" style="margin-bottom:12px">"Will be created"</div>
                    <table class="kv2">
                        <tr><td>"Name"</td><td class="mono">{move || if name.get().is_empty() { "—".into() } else { name.get() }}</td></tr>
                        <tr><td>"From"</td><td>{move || match start.get().as_str() { "copy" => format!("a copy of {}", label(&from.get())), "template" => format!("template {}", tpl.get()), "file" => "a folder".into(), _ => "empty".into() }}</td></tr>
                        <tr><td>"Services"</td><td class="mono">{move || {
                            let n = match start.get().as_str() { "copy" if has("services") => api::n(&source.get()["counts"], "services"), "template" => api::n(&template.get(), "services").max(1), _ => 2 };
                            format!("*.{} ({n})", base()) }}</td></tr>
                        <tr><td>"Apps reach it"</td><td class="mono">{move || format!("*.{}", base())}</td></tr>
                        <tr><td>"Browsers"</td><td class="mono">{move || format!("{}/", browsers())}</td></tr>
                        <tr><td>"MCP"</td><td class="mono">{move || format!("{}/mcp", browsers())}</td></tr>
                        <tr><td>"Expires"</td><td class="mono">{expiry_text}</td></tr>
                        <tr><td>"Size"</td><td class="mono">{size}</td></tr>
                    </table>
                    <div class="small cap" style="margin-top:16px">{move || format!("Apps use the base domain (point *.{} at Stuntman in DNS, or use /e/{} without one). People and sign-in pages use the public URL. Connections lists every setting.", base(), nm())}</div>
                </div>
            </div>
        </div>
    }
}
