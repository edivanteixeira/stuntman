//! A15 · Files: FTP, FTPS and SFTP servers partners and the app exchange files on. Folders and
//! files, put a file as the partner, the logins and transfers, failures provoked.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

#[component]
pub fn Files() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let servers = RwSignal::new(Value::Null);
    let files = RwSignal::new(Vec::<Value>::new());
    let log = RwSignal::new(Vec::<Value>::new());
    let folder = RwSignal::new(String::new());
    let preview = RwSignal::new(Value::Null);
    let (put_path, put_text) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (new_name, new_user) = (RwSignal::new(String::new()), RwSignal::new("edi".to_string()));
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("files".into()));
    let cur = Memo::new(move |_| qm.get().get("s").or_else(|| servers.get()["servers"].as_array().and_then(|a| a.first().map(|x| s(x, "name")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let c = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/files/servers").await { servers.set(v) }
            if !c.is_empty() {
                if let Ok(v) = api::get(&format!("/files/servers/{}/files", enc(&c))).await { files.set(v["files"].as_array().cloned().unwrap_or_default()) }
                if let Ok(v) = api::get(&format!("/files/transfers?server={}", enc(&c))).await { log.set(v["transfers"].as_array().cloned().unwrap_or_default()) }
            }
        });
    });
    let srv = Memo::new(move |_| servers.get()["servers"].as_array().and_then(|a| a.iter().find(|x| s(x, "name") == cur.get()).cloned()).unwrap_or(Value::Null));
    let open = move |p: String| { let c = cur.get_untracked(); leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/files/servers/{}/file?path={}", enc(&c), enc(&p))).await { preview.set(v) } }) };
    let put = move |_| {
        let (c, p, t) = (cur.get_untracked(), put_path.get_untracked(), put_text.get_untracked());
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/files/servers/{}/put", enc(&c)), json!({"path": p, "text": t})).await { Ok(_) => ui::toast("Put on the server, as the partner"), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |k: &'static str| { let c = cur.get_untracked(); leptos::task::spawn_local(async move { match api::send("PUT", &format!("/files/servers/{}", enc(&c)), json!({"provoke": k})).await { Ok(_) => ui::toast(if k == "none" { "Back to normal" } else { "Next sessions get it" }), Err(e) => ui::toast_err(e) } }) };
    let create = move |_| {
        let (nm, u) = (new_name.get_untracked(), new_user.get_untracked());
        leptos::task::spawn_local(async move { match api::send("PUT", &format!("/files/servers/{}", enc(&nm)), json!({"users": [{"user": u}], "folders": ["/in", "/out"]})).await { Ok(_) => ui::toast("Server created"), Err(e) => ui::toast_err(e) } });
    };
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Files"</div><div class="small cap">"FTP, FTPS, SFTP"</div></div>
                {move || servers.get()["servers"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let nm = s(&x, "name"); let n2 = nm.clone(); view! {
                    <a class="mrow" class:on=move || cur.get() == n2 href=format!("/ftp?s={}", enc(&nm))><div class="row"><span>{s(&x, "name")}</span><span class="grow"></span><span class="mono small cap">{n(&x, "files").to_string()}</span></div>
                        <div class="small cap">{if s(&x, "provoke").is_empty() { "SFTP · FTPS · FTP".to_string() } else { format!("provoked: {}", s(&x, "provoke")) }}</div></a> } }).collect_view()}
                <div class="stack" style="padding:12px 20px"><input class="input" placeholder="new server: edi-acme" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e))/>
                    <input class="input" placeholder="user" prop:value=move || new_user.get() on:input=move |e| new_user.set(event_target_value(&e))/>
                    <button class="btn small" on:click=create>"+ New server"</button></div>
            </div>
            <div class="page">
                <AreaHead kind=Signal::derive(move || { let c = srv.get()["connect"].clone(); format!("Files  ·  {}  ·  {}  ·  FTPS with AUTH TLS", s(&c, "sftp"), s(&c, "ftp")) })
                    title=Signal::derive(move || cur.get()) tabs=vec![("files", "Files", format!("/ftp?s={}", enc(&cur.get_untracked()))), ("transfers", "Transfers", format!("/ftp?s={}&tab=transfers", enc(&cur.get_untracked())))]
                    counts=Signal::derive(move || vec![("files", files.get().iter().filter(|f| f["dir"] == false).count().to_string()), ("transfers", log.get().len().to_string())]) active=if tab.get_untracked() == "transfers" { "transfers" } else { "files" }/>
                <div class="body" style="overflow:auto">
                    {move || (tab.get() == "transfers").then(|| view! {
                        <div style="padding:16px 24px;width:100%"><table class="tbl"><tr><th>"When"</th><th>"Who"</th><th>"Protocol"</th><th>"What"</th><th>"Path"</th><th>"Size"</th><th>"Result"</th></tr>
                            {log.get().into_iter().map(|t| view! { <tr><td class="mono">{api::hms(n(&t, "at"))}</td><td>{s(&t, "user")}</td><td>{s(&t, "proto")}</td><td>{s(&t, "op")}</td><td class="mono">{s(&t, "path")}</td>
                                <td class="mono">{api::kb(n(&t, "size"))}</td><td class={if t["ok"].as_bool().unwrap_or(false) { "ok" } else { "danger" }}>{s(&t, "detail")}</td></tr> }).collect_view()}</table></div> })}
                    {move || (tab.get() != "transfers").then(|| view! {
                        <div class="list" style="width:260px">
                            <div class="caps" style="padding:16px 20px 6px">"Folders"</div>
                            {files.get().into_iter().filter(|f| f["dir"] == true).map(|f| { let p = s(&f, "path"); let p2 = p.clone(); view! {
                                <div class="mrow" class:on=move || folder.get() == p on:click=move |_| folder.set(p2.clone())><span class="mono">{s(&f, "path")}</span></div> } }).collect_view()}
                        </div>
                        <div class="pane" style="padding:16px 24px;overflow:auto">
                            <table class="tbl"><tr><th>"File"</th><th>"Size"</th><th>"Modified"</th></tr>
                                {files.get().into_iter().filter(|f| f["dir"] == false && (folder.get().is_empty() || s(f, "path").starts_with(&format!("{}/", folder.get())))).map(|f| { let p = s(&f, "path"); view! {
                                    <tr style="cursor:pointer" on:click=move |_| open(p.clone())><td class="mono">{s(&f, "path")}</td><td class="mono">{api::kb(n(&f, "size"))}</td><td class="mono">{api::ago(n(&f, "modified"))}</td></tr> } }).collect_view()}</table>
                            {move || { let pv = preview.get(); (!pv.is_null()).then(|| view! { <div class="caps" style="margin-top:14px">{s(&pv, "path")}</div>
                                <pre class="yaml" style="white-space:pre-wrap;max-height:260px;overflow:auto">{if pv["text"].is_string() { s(&pv, "text") } else { format!("(binary, {} bytes)", n(&pv, "size")) }}</pre> }) }}
                            <div class="caps" style="margin-top:16px">"Put a file here · as if the partner dropped it"</div>
                            <div class="stack" style="max-width:620px"><input class="input mono" placeholder="/in/notfis/NOTFIS_1042.txt" prop:value=move || put_path.get() on:input=move |e| put_path.set(event_target_value(&e))/>
                                <textarea class="input mono" rows="4" placeholder="content" prop:value=move || put_text.get() on:input=move |e| put_text.set(event_target_value(&e))></textarea>
                                <div><button class="btn primary small" on:click=put>"Put the file"</button></div></div>
                            <div class="caps" style="margin-top:16px">"Provoke on the next sessions"</div>
                            <div class="row" style="gap:6px;flex-wrap:wrap">
                                {[("login_refused", "Login refused"), ("disk_full", "Disk full (552)"), ("permission_denied", "Permission denied"), ("drop_mid", "Drop after 4 KB"), ("slow", "Slow · 20 KB/s"), ("host_key_changed", "Host key changed"), ("none", "Normal")].into_iter().map(|(k, l)| view! {
                                    <button class="btn small" class:primary=move || s(&srv.get(), "provoke") == k on:click=move |_| provoke(k)>{l}</button> }).collect_view()}</div>
                            <div class="small cap mono" style="margin-top:12px">{move || { let c = &srv.get()["connect"]; format!("{}   ·   {}   ·   users: {}", s(c, "sftp"), s(c, "ftp"), srv.get()["users"].as_array().map(|a| a.iter().map(|u| s(u, "user")).collect::<Vec<_>>().join(", ")).unwrap_or_default()) }}</div>
                        </div> })}
                </div>
            </div>
        </div>
    }
}
