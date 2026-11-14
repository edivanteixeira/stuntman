//! S1 · Files › Objects: accounts and containers on the left, the folder in the middle (drop files
//! here), the object on the right with its preview, next read, properties and history.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{base64, files_of_drop, files_of_input, next_label, read_file, who, FilesHead};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Pop},
};

fn q_url(account: &str, container: &str, prefix: &str, sel: &str) -> String {
    let mut u = format!("/files?account={}&container={}", enc(account), enc(container));
    if !prefix.is_empty() {
        u.push_str(&format!("&prefix={}", enc(prefix)));
    }
    if !sel.is_empty() {
        u.push_str(&format!("&sel={}", enc(sel)));
    }
    u
}

fn path_enc(p: &str) -> String {
    p.split('/').map(enc).collect::<Vec<_>>().join("/")
}

fn sub_line(o: &Value) -> (String, bool) {
    let nr = s(o, "nextRead");
    if !nr.is_empty() {
        return (format!("next read: {}", next_label(&nr)), true);
    }
    let via = s(o, "via");
    if via == "seed" {
        let r = api::n(o, "reads");
        return (format!("from seed  ·  {}", if r == 0 { "never read".into() } else { format!("{r} reads") }), false);
    }
    let mut t = if via == "ui" || via == "api" || via == "mcp" { format!("added by {}", if via == "ui" { "you".into() } else { s(o, "by") }) } else { s(o, "ctype") };
    if o["truncated"] == true {
        t.push_str("  ·  truncated on purpose");
    } else {
        let r = api::n(o, "reads");
        if r > 0 {
            t.push_str(&format!("  ·  {r} read{}", if r == 1 { "" } else { "s" }));
        }
    }
    (t, false)
}

/// "14:41" today, "Nov 11" before.
fn when(ms: i64) -> String {
    if api::now() as i64 - ms < 86_400_000 { api::hm(ms) } else { crate::environments::day(ms) }
}

#[component]
pub fn Objects() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let qm = use_query_map();
    let q = RwSignal::new(String::new());
    let accounts = RwSignal::new(Vec::<Value>::new());
    let folder = RwSignal::new(Value::Null);
    let obj = RwSignal::new(Value::Null);
    let uploading = RwSignal::new(Vec::<(String, f64)>::new());
    let dragging = RwSignal::new(false);
    let reset = RwSignal::new(false);
    let del = RwSignal::new(false);
    let rm_acct = RwSignal::new(String::new());
    let rm_open = RwSignal::new(false);
    let cur = move |k: &str| qm.get().get(k).unwrap_or_default();
    // The first account and container when the URL names none.
    let account = Memo::new(move |_| { let a = cur("account"); if a.is_empty() { accounts.get().first().map(|x| s(x, "account")).unwrap_or_default() } else { a } });
    let container = Memo::new(move |_| {
        let c = cur("container");
        if !c.is_empty() { return c; }
        accounts.get().iter().find(|x| s(x, "account") == account.get()).and_then(|x| x["containers"].get(0).map(|c| s(c, "name"))).unwrap_or_default()
    });
    let prefix = Memo::new(move |_| cur("prefix"));
    let sel = Memo::new(move |_| cur("sel"));
    let kind = Memo::new(move |_| accounts.get().iter().find(|x| s(x, "account") == account.get()).map(|x| s(x, "kind")).unwrap_or_default());

    let load_accounts = move || leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/storage/accounts").await {
            accounts.set(v["accounts"].as_array().cloned().unwrap_or_default());
        }
    });
    let load_folder = move || {
        let (a, c, p) = (account.get_untracked(), container.get_untracked(), prefix.get_untracked());
        if a.is_empty() || c.is_empty() { folder.set(Value::Null); return; }
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/storage/{}/{}/objects?prefix={}", enc(&a), enc(&c), enc(&p))).await {
                folder.set(v);
            }
        });
    };
    let load_obj = move || {
        let (a, c, p) = (account.get_untracked(), container.get_untracked(), sel.get_untracked());
        if p.is_empty() { obj.set(Value::Null); return; }
        leptos::task::spawn_local(async move {
            obj.set(api::get(&format!("/storage/{}/{}/objects/{}", enc(&a), enc(&c), path_enc(&p))).await.unwrap_or(Value::Null));
        });
    };
    load_accounts();
    Effect::new(move |_| { account.track(); container.track(); prefix.track(); load_folder(); });
    Effect::new(move |_| { sel.track(); container.track(); load_obj(); });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let topic = s(&live.last.get_untracked(), "topic");
        if prev.is_some() && topic.starts_with("storage.") {
            load_accounts();
            load_folder();
            load_obj();
        }
        t
    });

    let go = {
        let nav = nav.clone();
        move |a: String, c: String, p: String, sel: String| nav(&q_url(&a, &c, &p, &sel), Default::default())
    };
    let upload = move |files: Vec<web_sys::File>| {
        let (a, c, p) = (account.get_untracked(), container.get_untracked(), prefix.get_untracked());
        for f in files {
            let name = f.name();
            let size = f.size();
            uploading.update(|u| u.push((name.clone(), size)));
            let (a, c, p) = (a.clone(), c.clone(), p.clone());
            leptos::task::spawn_local(async move {
                let r = match read_file(f).await {
                    Ok(bytes) => api::send("PUT", &format!("/storage/{}/{}/objects/{}", enc(&a), enc(&c), path_enc(&format!("{p}{name}"))), json!({"content_base64": base64(&bytes)})).await.map(|_| ()),
                    Err(e) => Err(e),
                };
                uploading.update(|u| u.retain(|x| x.0 != name));
                match r { Ok(_) => ui::toast(format!("{name} added by you")), Err(e) => ui::toast_err(e) }
            });
        }
    };
    let up2 = upload.clone();
    let set_reach = move |acct: String, mode: &'static str| leptos::task::spawn_local(async move {
        match api::send("PUT", &format!("/storage/{}/reachability", enc(&acct)), json!({"mode": mode})).await {
            Ok(_) => ui::toast(if mode == "ok" { "Reachable again".to_string() } else { format!("{acct} answers {mode} to everything") }),
            Err(e) => ui::toast_err(e),
        }
    });

    view! {
        <div class="page">
            <FilesHead active="objects" q=q placeholder="Name, path, MD5…" right=move || view! {
                <label class="btn">"Upload"<input type="file" multiple style="display:none" on:change=move |e| upload(files_of_input(&e))/></label>
            }/>
            <div class="body">
                <div class="list narrow tree">
                    <div class="caps section" style="padding:18px 24px 8px">"Containers"</div>
                    {let go = go.clone(); move || {
                        let go = go.clone();
                        accounts.get().into_iter().filter(|a| s(a, "account") == account.get()).map(|a| {
                            let name = s(&a, "account");
                            let kind = if s(&a, "kind") == "s3" { "S3" } else { "Azure Blob" };
                            let menu = RwSignal::new(false);
                            let reach = a["reachability"].clone();
                            let reach2 = reach.clone();
                            let settings_href = format!("/services/{}?account={}", s(&a, "kind"), enc(&name));
                            let (_n1, n2, n3, n4, n5, n6) = (name.clone(), name.clone(), name.clone(), name.clone(), name.clone(), name.clone());
                            let conn = s(&a, "connection");
                            let svc = s(&a, "kind");
                            let go2 = go.clone();
                            view! {
                                <div class="acct">
                                    <div class="row"><span class="grow nm">{name.clone()}</span>
                                        <span class="anchor"><button class="link plain" on:click=move |_| menu.set(true)>"⋯"</button>
                                            <Pop open=menu right=true>
                                                <div class="mi" on:click={ let (conn, svc) = (conn.clone(), svc.clone()); move |_| ui::copy(&conn, if svc == "s3" { "S3 settings" } else { "Connection string" }) }>"Copy connection string"</div>
                                                <a class="mi" href=settings_href.clone()>"Account settings"</a>
                                                <div class="mh caps">"Make it unreachable"</div>
                                                <div class="mi" on:click={ let n2 = n2.clone(); move |_| set_reach(n2.clone(), "timeout") }>"Timeout · no answer"</div>
                                                <div class="mi" on:click={ let n3 = n3.clone(); move |_| set_reach(n3.clone(), "503") }>"503 · service unavailable"</div>
                                                <div class="mi" on:click={ let n4 = n4.clone(); move |_| set_reach(n4.clone(), "401") }>"401 · key rejected"</div>
                                                {(!reach2.is_null()).then(|| { let n = n6.clone(); view! { <div class="mi" on:click=move |_| set_reach(n.clone(), "ok")>"Back to normal"</div> } })}
                                                <div class="rule"></div>
                                                <div class="mi danger" on:click={ let n5 = n5.clone(); move |_| { rm_acct.set(n5.clone()); rm_open.set(true); } }>"Remove account"</div>
                                            </Pop></span></div>
                                    <div class="small cap">{format!("{kind} · {} {}", a["containers"].as_array().map(|x| x.len()).unwrap_or(0), if kind == "S3" { "bucket(s)" } else { "container(s)" })}</div>
                                    {(!reach.is_null()).then(|| view! { <div class="small provoked">{format!("unreachable: {} · by {}", s(&reach, "mode"), who(&s(&reach, "by")))}</div> })}
                                </div>
                                {a["containers"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                                    let (an, cn) = (name.clone(), s(&c, "name"));
                                    let (an2, cn2) = (an.clone(), cn.clone());
                                    let go = go2.clone();
                                    view! {
                                        <div class="crow" class:on=move || account.get() == an && container.get() == cn on:click=move |_| go(an2.clone(), cn2.clone(), String::new(), String::new())>
                                            <span class="mono">{s(&c, "name")}</span><span class="mono small cap">{api::thousands(api::n(&c, "objects"))}</span>
                                        </div>
                                    }
                                }).collect_view()}
                            }
                        }).collect_view()
                    }}
                    <div class="seed">
                        <div class="caps">"Seed"</div>
                        {move || {
                            let a = accounts.get().into_iter().find(|x| s(x, "account") == account.get()).unwrap_or(Value::Null);
                            let sd = a["seed"].clone();
                            view! {
                                <div class="mono small t2" style="margin-top:8px">{format!("{}  ·  {} files", s(&sd, "path"), api::n(&sd, "files"))}</div>
                                <div class="small cap">{if sd["loadedAt"].is_null() { "not loaded yet".to_string() } else { format!("Loaded {}  ·  {} changed since", api::hm(api::n(&sd, "loadedAt")), api::n(&sd["changes"], "total")) }}</div>
                                <button class="link" style="margin-top:8px;font-size:13px" on:click=move |_| reset.set(true)>"Reset to seed"</button>
                            }
                        }}
                    </div>
                </div>
                <div class="pane files" on:dragover=move |e: web_sys::DragEvent| { e.prevent_default(); dragging.set(true); } on:dragleave=move |_| dragging.set(false)
                    on:drop=move |e: web_sys::DragEvent| { e.prevent_default(); dragging.set(false); up2(files_of_drop(&e)); }>
                    {move || {
                        let f = folder.get();
                        if accounts.get().is_empty() {
                            return view! { <div class="empty"><h3>"No storage accounts yet"</h3><div>"Add an Azure Blob or S3 account; the app then points its connection string or endpoint at Stuntman."</div>
                                <div style="margin-top:16px" class="row" ><a class="btn" href="/services/azure-blob?new=1">"Azure Blob account"</a><a class="btn" href="/services/s3?new=1">"S3 account"</a></div></div> }.into_any();
                        }
                        if f.is_null() { return view! { <div class="empty cap">"Loading…"</div> }.into_any(); }
                        let (a, c, p) = (account.get(), container.get(), prefix.get());
                        let filter = q.get().to_lowercase();
                        let crumbs: Vec<String> = p.split('/').filter(|x| !x.is_empty()).map(String::from).collect();
                        let go = go.clone();
                        let (go_c, go_f, go_o) = (go.clone(), go.clone(), go.clone());
                        let objs: Vec<Value> = f["objects"].as_array().cloned().unwrap_or_default().into_iter().filter(|o| filter.is_empty() || s(o, "path").to_lowercase().contains(&filter) || s(o, "md5").to_lowercase().contains(&filter)).collect();
                        let folders = f["folders"].as_array().cloned().unwrap_or_default();
                        let empty = objs.is_empty() && folders.is_empty();
                        view! {
                            <div class="crumbs row">
                                <span class="mono link-ish" on:click={ let (a, c) = (a.clone(), c.clone()); let go = go_c.clone(); move |_| go(a.clone(), c.clone(), String::new(), String::new()) }>{c.clone()}</span>
                                {crumbs.iter().enumerate().map(|(i, x)| {
                                    let to = crumbs[..=i].join("/") + "/";
                                    let last = i + 1 == crumbs.len();
                                    let (a, c, go) = (a.clone(), c.clone(), go_c.clone());
                                    view! { <span class="cap">" / "</span><span class="mono" class:strong=last class:link-ish=!last on:click=move |_| go(a.clone(), c.clone(), to.clone(), String::new())>{x.clone()}</span> }
                                }).collect_view()}
                                <span class="cap">" /"</span><span class="grow"></span>
                                <span class="small cap">{format!("{} objects  ·  {}", api::thousands(api::n(&f, "totalObjects")), api::kb(api::n(&f, "totalBytes")))}</span>
                            </div>
                            <div class="ftable">
                                <div class="fhead caps"><span class="grow">"Name"</span><span class="w-size">"Size"</span><span class="w-mod">"Modified ↓"</span><span class="w-by">"By"</span></div>
                                {folders.into_iter().map(|d| {
                                    let (a, c, to, go) = (a.clone(), c.clone(), s(&d, "path"), go_f.clone());
                                    view! { <div class="frow" on:click=move |_| go(a.clone(), c.clone(), to.clone(), String::new())>
                                        <div class="grow"><div class="mono">{format!("{}/", s(&d, "name"))}</div><div class="small cap">{format!("folder  ·  {} objects", api::n(&d, "objects"))}</div></div>
                                        <span class="w-size mono cap">"—"</span><span class="w-mod mono">{when(api::n(&d, "at"))}</span><span class="w-by"></span></div> }
                                }).collect_view()}
                                {objs.into_iter().map(|o| {
                                    let (line, armed) = sub_line(&o);
                                    let path = s(&o, "path");
                                    let p2 = path.clone();
                                    let (a, c, pre, go) = (a.clone(), c.clone(), p.clone(), go_o.clone());
                                    let by = if s(&o, "via") == "ui" { "you".to_string() } else { s(&o, "by") };
                                    view! { <div class="frow" class:on=move || sel.get() == p2 on:click=move |_| go(a.clone(), c.clone(), pre.clone(), path.clone())>
                                        <div class="grow"><div class="mono">{s(&o, "name")}</div><div class="small" class:attn=armed class:cap=!armed>{line}</div></div>
                                        <span class="w-size mono">{api::kb(api::n(&o, "size"))}</span><span class="w-mod mono">{when(api::n(&o, "at"))}</span><span class="w-by">{by}</span></div> }
                                }).collect_view()}
                            </div>
                            {empty.then(|| view! { <div class="callout" style="margin:20px 24px">
                                <div class="t">{format!("Nothing in {c} yet")}</div>
                                <div class="t2">"The app has not written here. Drop a file to prepare a test, or put files in the seed folder so every reset brings them back."</div></div> })}
                            {move || uploading.get().into_iter().map(|(n, size)| view! {
                                <div class="callout row" style="margin:12px 24px"><span class="mono grow">{n}</span><span class="small cap">{format!("uploading  ·  {}", api::kb(size as i64))}</span></div> }).collect_view()}
                            <div class="drop" class:over=move || dragging.get()>
                                <div>{format!("Drop files to add them to {c}/{p}")}</div>
                                <div class="small cap">"They show as added by you, never as sent by the app"</div>
                            </div>
                        }.into_any()
                    }}
                </div>
                <div class="side envside objside">
                    {move || {
                        let o = obj.get();
                        if o.is_null() { return view! { <div class="cap" style="margin-top:40px;text-align:center">"Pick a file to see it, provoke its next read and read its history."</div> }.into_any(); }
                        detail(o, kind.get(), del).into_any()
                    }}
                </div>
            </div>
            <Confirm open=reset question=Signal::derive(|| "Reset every storage account to the seed?".to_string())
                detail=Signal::derive(move || {
                    let (mut app, mut people, mut dels) = (0, 0, 0);
                    for a in accounts.get() { let c = &a["seed"]["changes"]; app += api::n(c, "app"); people += api::n(c, "people"); dels += api::n(c, "deletes"); }
                    format!("{} changes are lost: {app} files the app uploaded, {people} files you or agents added and {dels} deletes. Links and events stay in the history. Only this environment; others keep their files.", app + people + dels)
                })
                action="Reset to seed" on_yes=move |_| leptos::task::spawn_local(async move {
                    match api::send("POST", "/storage/reset", json!({"to": "seed"})).await { Ok(_) => ui::toast("Back to the seed"), Err(e) => ui::toast_err(e) }
                })/>
            <Confirm open=del question=Signal::derive(move || format!("Delete {}?", sel.get().rsplit('/').next().unwrap_or("")))
                detail=Signal::derive(|| "The delete stays in the history and sends BlobDeleted to the subscriptions.".to_string())
                action="Delete" on_yes=move |_| {
                    let (a, c, p) = (account.get_untracked(), container.get_untracked(), sel.get_untracked());
                    leptos::task::spawn_local(async move {
                        match api::send("DELETE", &format!("/storage/{}/{}/objects/{}", enc(&a), enc(&c), path_enc(&p)), json!({})).await { Ok(_) => ui::toast("Deleted"), Err(e) => ui::toast_err(e) }
                    });
                }/>
            <Confirm open=rm_open question=Signal::derive(move || format!("Remove {}?", rm_acct.get()))
                detail=Signal::derive(|| "The account, its objects and links go from this environment. The seed folder is not changed.".to_string())
                action="Remove account" on_yes=move |_| {
                    let n = rm_acct.get_untracked();
                    let kind = accounts.get_untracked().iter().find(|x| s(x, "account") == n).map(|x| s(x, "kind")).unwrap_or_default();
                    rm_acct.set(String::new());
                    leptos::task::spawn_local(async move {
                        match api::send("DELETE", &format!("/services/{kind}/accounts/{}", enc(&n)), json!({})).await { Ok(_) => ui::toast("Account removed"), Err(e) => ui::toast_err(e) }
                    });
                }/>
        </div>
    }
}

fn detail(o: Value, kind: String, del: RwSignal<bool>) -> impl IntoView {
    let (a, c, p) = (s(&o, "account"), s(&o, "container"), s(&o, "path"));
    let base = format!("/storage/{}/{}", enc(&a), enc(&c));
    let content = api::url(&s(&o, "contentUrl"));
    let ctype = s(&o, "ctype");
    let copy_menu = RwSignal::new(false);
    let more = RwSignal::new(false);
    let next_menu = RwSignal::new(false);
    let nr = o["nextRead"].clone();
    let url = s(&o, "url");
    let (b1, b2, b3, b4, b5) = (base.clone(), base.clone(), base.clone(), base.clone(), base.clone());
    let (p1, p2, p3, p4, p5) = (p.clone(), p.clone(), p.clone(), p.clone(), p.clone());
    let set_next = move |r: &'static str| {
        let (b, p) = (b1.clone(), p1.clone());
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("{}/next-read/{}", b.replace("/storage/", "/storage/"), path_enc(&p)), json!({"result": r, "times": 1})).await {
                Ok(_) => ui::toast(if r == "normal" { "Next read: normal".to_string() } else { format!("Armed: the next read answers {}", next_label(r)) }),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let set_next2 = set_next.clone();
    let make_link = move |valid: &'static str| {
        let (b, p) = (b2.clone(), p2.clone());
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("{b}/links/{}", path_enc(&p)), json!({"valid_for": valid})).await {
                Ok(v) => ui::copy(&s(&v, "url"), &format!("Read link ({valid})")),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let mk2 = make_link.clone();
    let truncate = move |_| {
        let (b, p) = (b3.clone(), p3.clone());
        leptos::task::spawn_local(async move {
            let r = async {
                let bytes = gloo_net::http::Request::get(&api::url(&format!("/api{b}/content/{}", path_enc(&p)))).send().await.map_err(|e| e.to_string())?.binary().await.map_err(|e| e.to_string())?;
                let half = &bytes[..bytes.len() / 2];
                api::send("PUT", &format!("{b}/objects/{}", path_enc(&p)), json!({"content_base64": base64(half), "truncated": true})).await
            }.await;
            match r { Ok(_) => ui::toast("Cut off: the second half is gone"), Err(e) => ui::toast_err(e) }
        });
    };
    let replace = move |e: leptos::ev::Event| {
        let files = files_of_input(&e);
        let (b, p) = (b4.clone(), p4.clone());
        if let Some(f) = files.into_iter().next() {
            leptos::task::spawn_local(async move {
                let r = match read_file(f).await { Ok(d) => api::send("PUT", &format!("{b}/objects/{}", path_enc(&p)), json!({"content_base64": base64(&d)})).await.map(|_| ()), Err(e) => Err(e) };
                match r { Ok(_) => ui::toast("Replaced"), Err(e) => ui::toast_err(e) }
            });
        }
    };
    let resend = move |_| {
        let (c2, p) = (c.clone(), p5.clone());
        leptos::task::spawn_local(async move {
            let subs = api::get("/storage/subscriptions").await.unwrap_or(Value::Null);
            let mut n = 0;
            for sb in subs["subscriptions"].as_array().cloned().unwrap_or_default() {
                if api::send("POST", "/storage/events", json!({"subscription": s(&sb, "name"), "type": "created", "path": format!("{c2}/{p}")})).await.is_ok() { n += 1; }
            }
            ui::toast(format!("BlobCreated sent again to {n} subscription(s)"));
        });
    };
    let _ = b5;
    let kind2 = kind.clone();
    let blob_label = if kind == "s3" { "Object URL" } else { "Blob URL" };
    let event_label = if kind == "s3" { "Send ObjectCreated again" } else { "Send BlobCreated again" };
    let link_label = if kind == "s3" { "Read link, 1 hour (presigned)" } else { "Read link, 1 hour (SAS)" };
    let path_text = format!("{}/{}", c_of(&o), p);
    let preview = if ctype.starts_with("image/") {
        view! { <div class="preview small-prev"><img src=content.clone() style="max-width:100%;max-height:220px"/></div> }.into_any()
    } else if ctype == "application/pdf" || ctype.starts_with("text/") {
        view! { <div class="preview small-prev"><iframe src=content.clone() style="width:100%;height:220px;border:0;background:#fff"></iframe></div> }.into_any()
    } else {
        view! { <div class="preview small-prev noprev"><div>{format!("No preview for {ctype}")}</div><div class="small cap">{format!("{}  ·  Download to open", api::kb(api::n(&o, "size")))}</div></div> }.into_any()
    };
    let mut hist: Vec<(i64, String, String, bool)> = vec![];
    for h in o["history"].as_array().cloned().unwrap_or_default() {
        let st = api::n(&h, "status");
        let op = s(&h, "op");
        let caller = if ["ui", "api", "mcp"].contains(&s(&h, "via").as_str()) { who(&s(&h, "caller")) } else { s(&h, "caller") };
        let what = match (op.as_str(), st) {
            ("put" | "copy", s2) if s2 < 300 => format!("Uploaded by {}", if s(&h, "via") == "ui" { "you".to_string() } else { caller.clone() }),
            ("get", s2) if s2 < 300 => format!("Read by {caller}{}", if s(&h, "via") == "sas" || s(&h, "via") == "presigned" { " through the link" } else { "" }),
            ("head", s2) if s2 < 300 => format!("Checked by {caller}"),
            ("delete", s2) if s2 < 300 => format!("Deleted by {caller}"),
            (_, 0) => format!("{} got no answer", caller),
            (o2, s2) => format!("{} refused with {s2}{}", match o2 { "put" => "Upload", "get" => "Read", "delete" => "Delete", _ => "Request" }, if s(&h, "provoked").is_empty() { String::new() } else { ", as provoked".into() }),
        };
        let prov = s(&h, "provoked");
        let meta = format!("{}  ·  {}  ·  {}{}", s(&h, "method"), if st == 0 { "—".to_string() } else { st.to_string() }, s(&h, "via").to_uppercase(), if prov.is_empty() { String::new() } else { format!("  ·  {prov}") });
        hist.push((api::n(&h, "at"), what, meta, !prov.is_empty() || st >= 400));
    }
    for e in o["events"].as_array().cloned().unwrap_or_default() {
        hist.push((api::n(&e, "at"), format!("{} sent to {}", s(&e, "type"), s(&e, "subscription")), format!("{}  ·  Events ›", s(&e, "status").replace('_', " ")), false));
    }
    for l in o["links"].as_array().cloned().unwrap_or_default() {
        hist.push((api::n(&l, "issuedAt"), format!("Read link issued, expires {}", api::hm(api::n(&l, "expiry"))), format!("{}  ·  Links ›", if s(&l, "kind") == "sas" { "SAS" } else { "presigned" }), false));
    }
    hist.sort_by(|x, y| x.0.cmp(&y.0));
    let versions = o["versions"].as_array().map(|v| v.len()).unwrap_or(1);
    view! {
        <h2 class="h2 mono">{s(&o, "name")}</h2>
        <div class="small cap">{format!("{}/{}  ·  {}  ·  {}", c_of(&o), p.rsplit_once('/').map(|x| format!("{}/", x.0)).unwrap_or_default(), api::kb(api::n(&o, "size")), ctype)}</div>
        <div class="row" style="margin:12px 0 14px;gap:8px">
            <a class="btn" href=content.clone() download=s(&o, "name")>"Download"</a>
            <span class="anchor"><button class="btn" on:click=move |_| copy_menu.set(true)>"Copy URL"<span class="small">"▾"</span></button>
                <Pop open=copy_menu>
                    <div class="mi" on:click={ let u = url.clone(); move |_| ui::copy(&u, blob_label) }><span>{blob_label}</span></div>
                    <div class="sub mono" style="padding:0 10px 6px;max-width:300px;overflow:hidden;text-overflow:ellipsis">{url.clone()}</div>
                    <div class="mi" on:click={ let f = make_link.clone(); move |_| f("1h") }>{link_label}</div>
                    <div class="mi" on:click={ let f = mk2.clone(); move |_| f("24h") }>"Read link, 24 hours"</div>
                    <div class="mi" on:click={ let t = path_text.clone(); move |_| ui::copy(&t, "Path") }>"Path for tests and YAML"</div>
                </Pop></span>
            <span class="anchor"><button class="btn icon" on:click=move |_| more.set(true)>"⋯"</button>
                <Pop open=more>
                    <label class="mi">"Replace with another file…"<input type="file" style="display:none" on:change=replace.clone()/></label>
                    <div class="mi" on:click=truncate.clone()>"Cut it off (truncate)"</div>
                    <div class="mi" on:click=resend.clone()>{event_label}</div>
                    <div class="rule"></div>
                    <div class="mi danger" on:click=move |_| del.set(true)>"Delete"</div>
                </Pop></span>
        </div>
        {preview}
        <div class="row" style="margin:16px 0 4px;gap:12px">
            <span style="width:84px">"Next read"</span>
            <span class="anchor"><button class="btn" class:armed=!nr.is_null() on:click=move |_| next_menu.set(true)>{if nr.is_null() { "Normal".to_string() } else { next_label(&s(&nr, "result")) }}<span class="small">"▾"</span></button>
                <Pop open=next_menu>
                    {[("normal", "Normal"), ("403", "403 Forbidden"), ("404", "404 Not found"), ("503", "503 Server busy"), ("slow:5s", "Slow · 5 s, then the file"), ("truncate", "Cut off halfway"), ("bad_md5", "Wrong checksum (Content-MD5)")].into_iter()
                        .map(|(v, l)| { let f = set_next2.clone(); view! { <div class="mi" on:click=move |_| f(v)>{l}</div> } }).collect_view()}
                    <div class="rule"></div><div class="mh small cap">"Once, then back to normal"</div>
                </Pop></span>
            {if nr.is_null() { view! { <span class="small cap">"once, then normal  ·  "</span><a class="link" href=format!("/services/{}/rules?path={}", if kind2 == "s3" { "s3" } else { "azure-blob" }, enc(&p))>"Make it a rule ›"</a> }.into_any() }
             else { let f = set_next.clone(); view! { <span class="small attn">{format!("armed  ·  fires on the next read  ·  by {}", who(&s(&nr, "by")))}</span><button class="link" on:click=move |_| f("normal")>"Cancel"</button> }.into_any() }}
        </div>
        <div class="caps" style="margin:20px 0 8px">"Properties"</div>
        <table class="kv2">
            <tr><td>"ETag"</td><td class="mono">{s(&o, "etag").trim_matches('"').to_string()}</td></tr>
            <tr><td>"Content-MD5"</td><td class="mono">{s(&o, "md5")}</td></tr>
            {o["meta"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| view! { <tr><td class="mono">{format!("{}{k}", if kind2 == "s3" { "x-amz-meta-" } else { "x-ms-meta-" })}</td><td class="mono">{v.as_str().unwrap_or("").to_string()}</td></tr> }).collect_view()}
            {o["headers"].as_object().cloned().unwrap_or_default().into_iter().map(|(k, v)| view! { <tr><td class="mono">{k}</td><td class="mono">{v.as_str().unwrap_or("").to_string()}</td></tr> }).collect_view()}
            <tr><td>"Versions"</td><td>{versions}</td></tr>
        </table>
        <div class="caps" style="margin:20px 0 8px">"History"</div>
        {hist.into_iter().map(|(at, what, meta, bad)| view! {
            <div class="hrow"><span class="mono small cap">{api::hms(at)}</span><div><div>{what}</div><div class="small" class:attn=bad class:cap=!bad>{meta}</div></div></div>
        }).collect_view()}
    }
}

fn c_of(o: &Value) -> String {
    s(o, "container")
}
