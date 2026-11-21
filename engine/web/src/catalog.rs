//! Catalog (K1–K3): the store of plugins (collections, categories, region), one plugin before
//! installing it, and importing your own from a folder, a .zip or a git URL.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui,
};

fn initials(t: &str) -> String {
    let w: Vec<&str> = t.split(|c: char| !c.is_alphanumeric()).filter(|x| !x.is_empty()).collect();
    match w.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect(),
        [a, b, ..] => format!("{}{}", a.chars().next().unwrap_or(' '), b.chars().next().unwrap_or(' ')).to_uppercase(),
    }
}

#[component]
fn Mono(title: String, region: String, #[prop(optional)] big: bool) -> impl IntoView {
    view! { <span class="mono-tile" class:br=region == "BR" class:big=big>{initials(&title)}</span> }
}

fn origin_region(p: &Value) -> String {
    let r = s(p, "region");
    if r.is_empty() { s(p, "origin") } else { format!("{}  ·  {r}", s(p, "origin")) }
}

fn env_name() -> String {
    let env = expect_context::<RwSignal<Value>>();
    s(&env.get_untracked(), "env").to_uppercase()
}

/// A declared page drawn small, from the plugin's description of it (no request is made).
#[component]
pub fn MiniPage(page: Value, people: Vec<Value>) -> impl IntoView {
    let fields: Vec<String> = page["fields"].as_array().cloned().unwrap_or_default().iter().map(|f| f.as_str().unwrap_or("").to_string()).collect();
    let push = page["push"] == true && !fields.iter().any(|f| f == "otp");
    let buttons: Vec<String> = page["buttons"].as_array().cloned().unwrap_or_default().iter().map(|b| b.as_str().unwrap_or("").to_string()).collect();
    view! {
        <div class="mini">
            <div class="mini-h"><span class="bar"></span><b>{s(&page, "title")}</b><span class="grow"></span><span class="small cap">"stand-in"</span></div>
            <div class="small t2">"acme-api wants to sign 1 document"</div>
            {fields.iter().any(|f| f == "person").then(|| people.iter().take(2).enumerate().map(|(i, p)| {
                let cert = s(p, "certificate");
                view! { <div class="mini-p" class:on=i == 0><span class="radio" class:on=i == 0></span>
                    <span class:attn=cert == "expired">{format!("{}  ·  {}", s(p, "name"), match cert.as_str() { "expired" => "certificate expired".to_string(), "none" => "no certificate".to_string(), _ => format!("{}, valid", if s(p, "kind").is_empty() { "e-CPF".into() } else { s(p, "kind") }) })}</span></div> }
            }).collect_view())}
            <div class="row" style="margin-top:10px">
                {fields.iter().any(|f| f == "otp").then(|| view! { <span class="mini-otp mono">"482  913"</span> })}
                <span class="grow"></span>
                {buttons.get(1).map(|d| view! { <span class="mini-btn">{d.clone()}</span> })}
                <span class="mini-btn primary">{buttons.first().cloned().unwrap_or("Authorize".into())}</span>
            </div>
            {push.then(|| view! { <div class="small cap" style="margin-top:8px">"or a push approval the app polls"</div> })}
        </div>
    }
}

/// K1: the store.
#[component]
pub fn Store() -> impl IntoView {
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let data = RwSignal::new(Value::Null);
    let q = RwSignal::new(String::new());
    let region = RwSignal::new("Any".to_string());
    let category = RwSignal::new("All".to_string());
    let source = RwSignal::new(String::new());
    let feature = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let (qq, rr, cc) = (q.get(), region.get(), category.get());
        leptos::task::spawn_local(async move {
            let path = format!("/plugins?query={}&region={}&category={}", enc(&qq), enc(&rr), enc(&if cc == "All" { String::new() } else { cc }));
            if let Ok(v) = api::get(&path).await {
                // The featured collection's page, drawn from its first plugin that has one.
                if feature.get_untracked().is_null() {
                    if let Some(name) = v["collections"][0]["plugins"].as_array().and_then(|a| a.iter().map(|x| s(x, "name")).find(|n| v["plugins"].as_array().map(|ps| ps.iter().any(|p| s(p, "name") == *n && api::n(p, "pages") > 0)).unwrap_or(false))) {
                        if let Ok(d) = api::get(&format!("/catalog/{}", enc(&name))).await { feature.set(d); }
                    }
                }
                data.set(v);
            }
        });
    });
    let install = move |name: String| {
        leptos::task::spawn_local(async move {
            match api::send("POST", "/plugins", json!({"plugin": name})).await {
                Ok(_) => ui::toast("Installed"),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let add_rest = move |name: String| {
        leptos::task::spawn_local(async move {
            match api::send("POST", "/catalog/collections/install", json!({"name": name})).await {
                Ok(v) => ui::toast(format!("Added {}", v["added"].as_array().map(|a| a.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default())),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let n1 = nav.clone();
    let n2 = nav.clone();
    let n3 = nav.clone();
    view! {
        <div class="page"><div class="pane cat">
            <div class="row cat-top">
                <div class="grow"><h1>"Catalog"</h1>
                    <div class="t2">"Stand-ins for the providers your app calls. Each plugin is a file you can read: its routes, answers, pages and synthetic people."</div></div>
                <input class="input" style="width:270px" placeholder="Search plugins, a host, a provider…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                <button class="btn" on:click=move |_| n1("/catalog/import", Default::default())>"Import…"</button>
            </div>
            {move || data.get()["collections"].as_array().and_then(|c| c.first().cloned()).map(|c| {
                let missing = api::n(&c, "total") - api::n(&c, "installed");
                let name = s(&c, "name");
                let nav = n2.clone();
                let f = feature.get();
                view! {
                    <div class="feature">
                        <div class="grow">
                            <div class="caps" style="color:var(--on-ink-2)">"Collection"</div>
                            <h2>{name.clone()}</h2>
                            <div class="fd">{s(&c, "description")}</div>
                            <div class="fchips">
                                {c["plugins"].as_array().cloned().unwrap_or_default().into_iter().map(|p| {
                                    let nav = nav.clone();
                                    let n = s(&p, "name");
                                    view! { <div class="fchip" class:in=p["installed"] == true on:click=move |_| nav(&format!("/catalog/{}", enc(&n)), Default::default())>
                                        <span class="mono">{initials(&s(&p, "title"))}</span><span>{format!("{}{}", if p["installed"] == true { "✓ " } else { "" }, s(&p, "title"))}</span></div> }
                                }).collect_view()}
                            </div>
                            <div class="row" style="margin-top:18px">
                                {if missing > 0 {
                                    let n = name.clone();
                                    view! { <button class="btn light" on:click=move |_| add_rest(n.clone())>{format!("Add the {missing} missing")}</button> }.into_any()
                                } else {
                                    let nav = nav.clone();
                                    view! { <button class="btn light" on:click=move |_| nav("/apis", Default::default())>"Open in HTTP APIs"</button> }.into_any()
                                }}
                                <span class="small" style="color:var(--on-ink-2)">{format!("{} of {} in {}  ·  one install each, nothing hidden", api::n(&c, "installed"), api::n(&c, "total"), env_name())}</span>
                            </div>
                        </div>
                        {(!f.is_null()).then(|| {
                            let pg = f["pages"][0].clone();
                            let people = f["people"].as_array().cloned().unwrap_or_default();
                            view! { <div class="fpage"><MiniPage page=pg people=people/><div class="small" style="color:var(--on-ink-2);margin-top:8px">"Declared in the plugin, drawn by Stuntman."</div></div> }
                        })}
                    </div>
                }
            })}
            <div class="row cat-filters">
                {move || data.get()["categories"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                    let n = s(&c, "name");
                    let n1 = n.clone();
                    view! { <button class="fpill" class:on=move || category.get() == n1 on:click=move |_| category.set(n.clone())>{format!("{}  {}", s(&c, "name"), api::n(&c, "count"))}</button> }
                }).collect_view()}
                <span class="grow"></span>
                <span class="small cap">"Region"</span>
                <div class="seg">{["Any", "Global", "Brazil"].into_iter().map(|r| view! { <button class:on=move || region.get() == r on:click=move |_| region.set(r.into())>{r}</button> }).collect_view()}</div>
            </div>
            <div class="row" style="margin:6px 0 10px"><span class="caps">{move || format!("{}  ·  {}", if category.get() == "All" { "HTTP APIs".to_string() } else { category.get() }, data.get()["plugins"].as_array().map(|a| a.len()).unwrap_or(0))}</span></div>
            <div class="cgrid">
                {move || {
                    let list = data.get()["plugins"].as_array().cloned().unwrap_or_default();
                    if list.is_empty() && !data.get().is_null() {
                        return view! { <div class="callout">{format!("No plugin for “{}”. Install an empty one that records, from HTTP APIs › + New service, or import one you wrote.", q.get())}</div> }.into_any();
                    }
                    list.into_iter().map(|p| {
                        let name = s(&p, "name");
                        let (na, nb) = (name.clone(), name.clone());
                        let nav = n3.clone();
                        let inst = install;
                        view! {
                            <div class="ccard" on:click=move |_| nav(&format!("/catalog/{}", enc(&na)), Default::default())>
                                <div class="row" style="gap:12px;align-items:flex-start"><Mono title=s(&p, "title") region=s(&p, "region")/>
                                    <div><div class="nm">{s(&p, "title")}</div><div class="small cap">{origin_region(&p)}</div></div></div>
                                <div class="t2 d">{s(&p, "description")}</div>
                                <div class="row">
                                    {p["badges"].as_array().cloned().unwrap_or_default().into_iter().map(|b| { let b = b.as_str().unwrap_or("").to_string(); let (pg, bl) = (b == "browser page", b == "billable"); view! { <span class="badge" class:page=pg class:bill=bl>{b}</span> } }).collect_view()}
                                    <span class="grow"></span>
                                    {if p["update"] == true { view! { <span class="small attn">"Update available"</span> }.into_any() }
                                     else if p["installed"] == true { view! { <span class="small ok">"Installed"</span> }.into_any() }
                                     else { view! { <button class="link" on:click=move |e| { e.stop_propagation(); inst(nb.clone()) }>"Install"</button> }.into_any() }}
                                </div>
                            </div>
                        }
                    }).collect_view().into_any()
                }}
            </div>
            <div class="byo">
                <div class="grow"><div class="nm">"Bring your own plugin"</div>
                    <div class="t2 small">"A folder, a .zip or a git URL. You read what it answers, the pages it draws and what it may touch before anything is installed."</div></div>
                <input class="input mono" style="width:360px" placeholder="https://git.example.com/acme/plugins.git" prop:value=move || source.get() on:input=move |e| source.set(event_target_value(&e))/>
                <button class="btn primary" on:click=move |_| nav(&format!("/catalog/import?source={}", enc(&source.get_untracked())), Default::default())>"Read it"</button>
            </div>
            <div class="small cap" style="margin-top:10px">{move || format!("Source: {}  ·  no ratings or download counts: nothing real to show yet.", s(&data.get(), "source"))}</div>
        </div></div>
    }
}

#[component]
fn Detail(d: Value, #[prop(optional)] import: bool) -> impl IntoView {
    let people = d["people"].as_array().cloned().unwrap_or_default();
    view! {
        <div class="caps" style="margin:22px 0 8px">"What it answers"</div>
        <div class="ktable">
            {d["routes"].as_array().cloned().unwrap_or_default().into_iter().map(|r| {
                let (m, p) = s(&r, "route").split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or_default();
                view! { <div class="krt"><span class="mono small t2" style="width:44px">{m}</span><span class="mono s12" style="width:300px">{p}</span><span class="t2 s12">{s(&r, "what")}</span></div> }
            }).collect_view()}
        </div>
        {d["pages"].as_array().filter(|a| !a.is_empty()).cloned().map(|pages| view! {
            <div class="caps" style="margin:26px 0 8px">{if import { "Pages it draws" } else { "The page it draws" }}</div>
            {pages.into_iter().map(|pg| { let pp = people.clone(); view! {
                <div class="row" style="align-items:stretch;gap:16px;margin-bottom:12px">
                    <div style="width:330px"><MiniPage page=pg.clone() people=pp/></div>
                    <pre class="yaml grow">{s(&pg, "yaml")}</pre>
                </div> } }).collect_view()}
            <div class="small cap">"Fields and buttons only, no HTML or scripts: every page looks like Stuntman, and agents fill it with authorize_as."</div>
        })}
        {(!people.is_empty()).then(|| view! {
            <div class="caps" style="margin:26px 0 8px">"People it brings"</div>
            {people.iter().map(|p| { let c = s(p, "certificate"); view! {
                <div class="row" style="gap:16px;margin-bottom:6px"><span style="width:130px;font-weight:500">{s(p, "name")}</span><span class="mono small cap" style="width:150px">{format!("CPF {}", s(p, "cpf"))}</span>
                    <span class="s12" class:ok=c.is_empty() || c == "valid" class:attn=c == "expired" class:t2=c == "none">{match c.as_str() { "expired" => "certificate expired".to_string(), "none" => "no certificate".to_string(), "" => "—".to_string(), _ => format!("{}, valid", if s(p, "kind").is_empty() { "certificate".into() } else { s(p, "kind") }) }}</span></div>
            } }).collect_view()}
            <div class="small cap">"Certificates are issued by the Stuntman Test CA when first used, so the app can verify every signature it gets back."</div>
        })}
        <div class="caps" style="margin:26px 0 8px">"Situations you can provoke"</div>
        <div class="row" style="flex-wrap:wrap;gap:6px">
            {d["situations"].as_array().cloned().unwrap_or_default().into_iter().map(|x| view! { <span class="fpill small" title=s(&x, "note")>{format!("{}{}", s(&x, "name"), if api::n(&x, "status") >= 400 { format!(" · {}", api::n(&x, "status")) } else { String::new() })}</span> }).collect_view()}
        </div>
    }
}

#[component]
fn SideInfo(d: Value) -> impl IntoView {
    view! {
        <div class="caps" style="margin:22px 0 8px">"Permissions"</div>
        {[("Network", "network"), ("Disk", "disk"), ("Scripts", "scripts")].into_iter().map(|(l, k)| view! { <div class="row s12" style="margin-bottom:4px"><span class="t2" style="width:70px">{l}</span><span>{s(&d["permissions"], k)}</span></div> }).collect_view()}
        <div class="caps" style="margin:22px 0 8px">"Files"</div>
        {d["files"].as_array().cloned().unwrap_or_default().into_iter().map(|f| view! { <div class="row s12" style="margin-bottom:4px"><span class="mono" style="width:130px">{s(&f, "name")}</span><span class="cap">{s(&f, "what")}</span></div> }).collect_view()}
    }
}

/// K2: one catalog plugin.
#[component]
pub fn PluginPage() -> impl IntoView {
    let p = use_params_map();
    let nav = use_navigate();
    let d = RwSignal::new(Value::Null);
    let installed = RwSignal::new(false);
    let svc = RwSignal::new(String::new());
    let all = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        let name = p.get().get("plugin").unwrap_or_default();
        svc.set(name.clone());
        leptos::task::spawn_local(async move {
            match api::get(&format!("/catalog/{}", enc(&name))).await { Ok(v) => d.set(v), Err(e) => ui::toast_err(e) }
            if let Ok(v) = api::get("/plugins").await {
                installed.set(v["plugins"].as_array().map(|a| a.iter().any(|x| s(x, "name") == name && x["installed"] == true)).unwrap_or(false));
                all.set(v);
            }
        });
    });
    let go = move || {
        let nav = nav.clone();
        let (plugin, service) = (s(&d.get_untracked(), "name"), svc.get_untracked());
        leptos::task::spawn_local(async move {
            match api::send("POST", "/plugins", json!({"plugin": plugin, "service": service})).await {
                Ok(v) => { ui::toast("Installed"); nav(&format!("/apis/{}/{}", enc(&s(&v, "service")), if d.get_untracked()["pages"].as_array().map(|a| !a.is_empty()).unwrap_or(false) { "pages" } else { "calls" }), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page"><div class="pane cat">
            {move || { let x = d.get(); (!x.is_null()).then(|| {
                let go = go.clone();
                view! {
                <div class="small"><a href="/catalog">"Catalog"</a><span class="cap">{format!("   ›   HTTP APIs   ›   {}", s(&x, "title"))}</span></div>
                <div class="row" style="gap:16px;margin-top:16px;align-items:flex-start">
                    <div class="grow" style="max-width:780px">
                        <div class="row" style="gap:16px"><Mono title=s(&x, "title") region=s(&x, "region") big=true/>
                            <div><h1 style="margin:0">{s(&x, "title")}</h1><div class="small cap">{format!("{}  ·  {}  ·  version {}", origin_region(&x), s(&x, "name"), &s(&x, "version")[..8.min(s(&x, "version").len())])}</div></div></div>
                        <div class="t2" style="margin-top:14px">{s(&x, "description")}</div>
                        <Detail d=x.clone()/>
                    </div>
                    <div class="side">
                        <div class="ibox">
                            <div class="caps">{format!("Install into {}", env_name())}</div>
                            {move || if installed.get() {
                                let n = svc.get();
                                view! { <div class="s12" style="margin:12px 0"><span class="ok">"Installed.  "</span><a href=format!("/apis/{}/calls", enc(&n))>"Open in HTTP APIs ›"</a></div>
                                    <div class="small cap">"Install again under another name:"</div> }.into_any()
                            } else { view! { <span></span> }.into_any() }}
                            <div class="small t2" style="margin:12px 0 6px">"Name"</div>
                            <input class="input mono" style="width:100%" prop:value=move || svc.get() on:input=move |e| svc.set(event_target_value(&e).trim().to_lowercase())/>
                            <div class="small t2" style="margin:12px 0 4px">"Then point the app's base URL at"</div>
                            <div class="mono small">{move || format!("http://127.0.0.1:8028/{}", svc.get())}</div>
                            <div class="small cap">"or <name>.<env>.localhost, or the real host with the Test CA"</div>
                            <button class="btn primary" style="width:100%;justify-content:center;margin-top:16px" on:click=move |_| go()>"Install"</button>
                        </div>
                        <SideInfo d=x.clone()/>
                        {x["collections"].as_array().filter(|a| !a.is_empty()).cloned().map(|cols| {
                            let members: Vec<Value> = all.get()["collections"].as_array().cloned().unwrap_or_default().into_iter().filter(|c| cols.contains(&c["name"])).flat_map(|c| c["plugins"].as_array().cloned().unwrap_or_default()).filter(|m| s(m, "name") != s(&x, "name")).collect();
                            view! { <div class="caps" style="margin:22px 0 8px">"Same collection"</div>
                                {members.into_iter().map(|m| view! { <div class="row s12" style="margin-bottom:4px"><a style="width:150px" href=format!("/catalog/{}", enc(&s(&m, "name")))>{s(&m, "title")}</a><span class="cap">{if m["installed"] == true { "installed" } else { "not installed" }}</span></div> }).collect_view()} }
                        })}
                    </div>
                </div>
            }})}}
        </div></div>
    }
}

/// K3: import your own plugin.
#[component]
pub fn Import() -> impl IntoView {
    let qm = use_query_map();
    let nav = use_navigate();
    let source = RwSignal::new(qm.get_untracked().get("source").unwrap_or_default());
    let path = RwSignal::new(String::new());
    let rf = RwSignal::new(String::new());
    let git = RwSignal::new(true);
    let res = RwSignal::new(Value::Null);
    let busy = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let read = move || {
        if source.get_untracked().trim().is_empty() { return; }
        busy.set(true);
        leptos::task::spawn_local(async move {
            match api::send("POST", "/catalog/imports", json!({"source": source.get_untracked(), "path": path.get_untracked(), "ref": rf.get_untracked()})).await {
                Ok(v) => { name.set(s(&v, "name")); res.set(v); }
                Err(e) => { res.set(Value::Null); ui::toast_err(e) }
            }
            busy.set(false);
        });
    };
    if !source.get_untracked().is_empty() {
        git.set(source.get_untracked().contains("://") || source.get_untracked().ends_with(".git"));
        read();
    }
    let install = move || {
        let nav = nav.clone();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/catalog/imports/{}/install", enc(&s(&res.get_untracked(), "id"))), json!({"service": name.get_untracked()})).await {
                Ok(v) => { ui::toast("Installed"); nav(&format!("/apis/{}/calls", enc(&s(&v, "service"))), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page"><div class="pane cat">
            <div class="small"><a href="/catalog">"Catalog"</a><span class="cap">"   ›   Import"</span></div>
            <h1 style="margin:10px 0 4px">"Import a plugin"</h1>
            <div class="t2">"Your own plugin, from a folder, a .zip or a git repository. Stuntman reads it and shows what it does; nothing runs until you install."</div>
            <div class="row" style="margin-top:18px;gap:8px">
                <div class="seg"><button class:on=move || !git.get() on:click=move |_| git.set(false)>"Folder or .zip"</button><button class:on=move || git.get() on:click=move |_| git.set(true)>"Git URL"</button></div>
                <input class="input mono grow" placeholder=move || if git.get() { "https://git.example.com/acme/plugins.git" } else { "/path/to/plugin-folder or plugin.zip" } prop:value=move || source.get() on:input=move |e| source.set(event_target_value(&e)) on:keydown=move |e| if e.key() == "Enter" { read() }/>
                <input class="input mono" style="width:140px" placeholder="path inside" prop:value=move || path.get() on:input=move |e| path.set(event_target_value(&e))/>
                {move || git.get().then(|| view! { <input class="input mono" style="width:100px" placeholder="branch" prop:value=move || rf.get() on:input=move |e| rf.set(event_target_value(&e))/> })}
                <button class="btn" disabled=move || busy.get() on:click=move |_| read()>{move || if busy.get() { "Reading…" } else if res.get().is_null() { "Read it" } else { "Read again" }}</button>
            </div>
            {move || { let r = res.get(); (!r.is_null()).then(|| { let install = install.clone(); view! {
                <div class="small cap" style="margin-top:6px">{format!("Read from {}", s(&r, "source"))}</div>
                {(r["from_catalog"] != true).then(|| view! { <div class="warnbar"><b>"Not from the catalog."</b>" Nobody reviewed these files. Read what they answer and what they may touch before installing."</div> })}
                <div class="row" style="gap:16px;margin-top:18px;align-items:flex-start">
                    <div class="grow" style="max-width:780px">
                        {(!r["plugin"].is_null()).then(|| view! { <div class="row" style="gap:14px"><Mono title=s(&r["plugin"], "title").chars().next().map(|_| s(&r["plugin"], "title")).unwrap_or(s(&r, "name")) region=s(&r["plugin"], "region")/>
                            <div><div style="font:700 20px var(--sans)">{if s(&r["plugin"], "title").is_empty() { s(&r, "name") } else { s(&r["plugin"], "title") }}</div><div class="small cap">{format!("yours  ·  {}", s(&r, "name"))}</div></div></div> })}
                        <div class="caps" style="margin:20px 0 8px">"What Stuntman checked"</div>
                        <div class="ktable">
                            {r["checks"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                                let (ok, warn) = (c["ok"] == true, c["warn"] == true);
                                view! { <div class="krt"><b class:ok=ok class:attn=warn class:danger=!ok && !warn style="width:20px">{if ok { "✓" } else if warn { "!" } else { "✕" }}</b>
                                    <span style="width:360px" class:danger=!ok && !warn class:attn=warn>{s(&c, "text")}</span><span class="t2 s12">{s(&c, "detail")}</span></div> }
                            }).collect_view()}
                        </div>
                        {(!r["plugin"].is_null()).then(|| view! { <Detail d=r["plugin"].clone() import=true/> })}
                    </div>
                    <div class="side">
                        <div class="ibox">
                            <div class="caps">{format!("Install into {}", env_name())}</div>
                            <div class="small t2" style="margin:12px 0 6px">"Name"</div>
                            <input class="input mono" style="width:100%" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e).trim().to_lowercase())/>
                            <button class="btn primary" style="width:100%;justify-content:center;margin-top:16px" disabled=r["can_install"] != true on:click=move |_| install()>
                                {if r["can_install"] != true { "Fix the file first" } else if r["checks"].as_array().map(|a| a.iter().any(|c| c["warn"] == true)).unwrap_or(false) { "Install anyway" } else { "Install" }}</button>
                            <div class="small cap" style="margin-top:10px">"Installing copies it into this environment as “yours”. Read again updates nothing until you install."</div>
                        </div>
                        {(!r["plugin"].is_null()).then(|| view! { <SideInfo d=json!({"permissions": r["plugin"]["permissions"], "files": r["files"]})/> })}
                    </div>
                </div>
            } }) }}
        </div></div>
    }
}
