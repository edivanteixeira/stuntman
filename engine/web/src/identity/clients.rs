//! I6 · OAuth clients: Stuntman as the client against the app's own OAuth server (MCP authorization
//! spec). Simulated clients in the column; the ten checks of the last run; the failing (or picked)
//! check with what was sent and got; Run again against the MCP server URL.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use crate::{
    api::{self, enc, s},
    ui::{self, Dialog},
};

fn icon(ok: &Value) -> (&'static str, &'static str) {
    match ok { Value::Bool(true) => ("✓", "ok"), Value::Bool(false) => ("✕", "danger"), _ => ("·", "cap") }
}

/// Why a failing check matters, in the frame's words.
fn why(n: i64) -> &'static str {
    match n {
        1 => "MCP clients can't find the authorization server: the 401 should name the protected resource metadata, or it must be at the well-known path.",
        2 => "Without the authorization server metadata a client can't find the authorize and token endpoints.",
        3 => "Clients that register themselves (DCR) or use a metadata URL can't connect without a manual step.",
        4 => "Any client could skip PKCE: an intercepted code could be redeemed by someone else.",
        5 => "Native and desktop clients come back to a loopback address; refusing it locks them out.",
        6 => "The person could not finish signing in at the app.",
        7 => "The code could not be exchanged for tokens.",
        8 => "The MCP spec asks for the resource indicator (RFC 8707): the token must name this MCP server, so a token stolen for one API can't be used on another.",
        9 => "The token the app issued is not accepted by its own MCP server.",
        10 => "A refresh token that keeps working after use lets a stolen copy live on.",
        _ => "",
    }
}

#[component]
pub fn Clients() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let nav = use_navigate();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let run = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(x) = api::get("/oauth-clients").await { data.set(x); } });
    });
    let cur = Memo::new(move |_| { let cs = data.get()["clients"].as_array().cloned().unwrap_or_default(); let q = qm.get().get("client").unwrap_or_default(); cs.iter().find(|c| s(c, "id") == q).cloned().or_else(|| cs.first().cloned()).unwrap_or(Value::Null) });
    // The run shown: ?run=, else the client's last one (fetched in full, refreshed live).
    Effect::new(move |_| {
        live.tick.track();
        let id = qm.get().get("run").unwrap_or_else(|| s(&cur.get()["lastRun"], "id"));
        if id.is_empty() { run.set(Value::Null); return; }
        leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/oauth-clients/runs/{}", enc(&id))).await { run.set(x); } });
    });
    let picked = RwSignal::new(0i64);
    let resource = RwSignal::new(String::new());
    Effect::new(move |_| { let r = s(&cur.get(), "resource"); if resource.get_untracked().is_empty() || !r.is_empty() { resource.set(r); } });
    let go = move |_| {
        let (c, res) = (s(&cur.get_untracked(), "id"), resource.get_untracked().trim().to_string());
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/oauth-clients/{}/runs", enc(&c)), json!({"resource_url": res})).await {
                Ok(x) => { picked.set(0); ui::toast("Running the checks…"); nav(&format!("/oauth-clients?client={}&run={}", enc(&c), enc(&s(&x, "run"))), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let add_open = RwSignal::new(false);
    let (a_id, a_kind, a_name) = (RwSignal::new(String::new()), RwSignal::new("dcr".to_string()), RwSignal::new(String::new()));
    let (a_cid, a_sec) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let add = move |_| {
        let (id, kind, name, cid, sec, res) = (a_id.get_untracked().trim().to_string(), a_kind.get_untracked(), a_name.get_untracked(), a_cid.get_untracked(), a_sec.get_untracked(), resource.get_untracked());
        let nav = use_navigate();
        leptos::task::spawn_local(async move {
            let mut b = json!({"kind": kind, "name": if name.trim().is_empty() { id.clone() } else { name.clone() }, "client_name": if name.trim().is_empty() { id.clone() } else { name }, "resource_url": res});
            if kind == "preregistered" { b["client_id"] = json!(cid); b["client_secret"] = json!(sec); }
            match api::send("PUT", &format!("/oauth-clients/{}", enc(&id)), b).await {
                Ok(_) => { add_open.set(false); ui::toast(format!("Client {id} added")); nav(&format!("/oauth-clients?client={}", enc(&id)), Default::default()); }
                Err(e) => ui::toast_err(e),
            }
        });
    };
    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <div class="titled">
                        <div class="kind">{move || format!("Identity  ·  OAuth clients  ·  {}", s(&env.get(), "env").to_uppercase())}</div>
                        <h1>"OAuth clients"</h1>
                    </div>
                    <span class="grow"></span>
                    <input class="input mono" style="width:320px" placeholder="MCP server, e.g. https://acme-api.stg.example.com/mcp" prop:value=move || resource.get() on:input=move |e| resource.set(event_target_value(&e))/>
                    <button class="btn primary" disabled=move || resource.get().trim().is_empty() || cur.get().is_null() on:click=go>{move || if run.get().is_null() { "Run the checks" } else { "Run again" }}</button>
                </div>
                <div class="tabs"><div class="tab on">"Clients"<span class="n">{move || data.get()["clients"].as_array().map(|a| a.len()).unwrap_or(0)}</span></div>
                    <a class="tab" href="/identity">"Identity providers ›"</a></div>
            </div>
            <div class="body">
                <div class="list narrow kvcol">
                    <div class="caps" style="padding:16px 20px 6px">"Simulated clients"</div>
                    {move || data.get()["clients"].as_array().cloned().unwrap_or_default().into_iter().map(|c| {
                        let id = s(&c, "id"); let (i2, i3) = (id.clone(), id.clone()); let nav = nav.clone();
                        let sub = match s(&c, "kind").as_str() { "metadata_url" => "client id is an https URL".to_string(), "preregistered" => "pre-registered (client id and secret)".to_string(), _ => "dynamic registration (RFC 7591)".to_string() };
                        let lr = &c["lastRun"];
                        let res = if lr.is_null() { String::new() } else { format!("  ·  last: {}", s(lr, "status")) };
                        view! { <div class="kvrow" class:on=move || s(&cur.get(), "id") == i2 on:click=move |_| nav(&format!("/oauth-clients?client={}", enc(&i3)), Default::default())>
                            <div>{s(&c, "name")}</div><div class="small cap">{format!("{sub}{res}")}</div></div> }
                    }).collect_view()}
                    <div style="padding:10px 20px"><button class="link" on:click=move |_| { a_id.set(String::new()); a_name.set(String::new()); add_open.set(true); }>"+ Add client"</button></div>
                    <div class="small cap" style="padding:4px 20px">"Here Stuntman is the client and the app is the authorization server: for apps that let AI assistants or partners sign in with OAuth."</div>
                </div>
                <div class="pane pad">
                    {move || {
                        let r = run.get();
                        let c = cur.get();
                        if r.is_null() { return view! { <div class="callout"><div class="t">"No run yet"</div>
                            <div class="t2">"Put the app's MCP server URL above and run the checks: ten steps of the MCP authorization spec, in order. The run pauses at the app's own sign-in page for a person (or an agent) to sign in."</div></div> }.into_any(); }
                        let checks = r["checks"].as_array().cloned().unwrap_or_default();
                        let passed = checks.iter().filter(|x| x["ok"] == json!(true)).count();
                        let st = s(&r, "status");
                        let auth = s(&r, "authorizeUrl");
                        view! {
                            <div style="font-weight:500">{format!("Against {}", s(&r, "resource"))}</div>
                            <div class="small" class:ok={st == "passed"} class:attn={st != "passed"}>{format!("{}  ·  {passed} of 10 checks passed{}", match st.as_str() { "running" => "running…".to_string(), "waiting" => "waiting for the sign-in".to_string(), "blocked" => "blocked by the environment".to_string(), _ => format!("last run {}", api::hm(api::n(&r, "at"))) }, if s(&c, "kind") == "metadata_url" { "  ·  client id is a URL" } else { "" })}</div>
                            {(st == "waiting").then(|| view! { <div class="callout attn-l" style="max-width:none;margin:16px 0 0"><div class="t">"Sign in to the app in the browser tab that opens"</div>
                                <div class="t2" style="margin:0">"The run pauses at the app's own sign-in page: Stuntman is the client, a person (or sign_in_as on the app's identity provider) signs in. It continues when the code comes back."</div>
                                <a class="link" style="margin-top:8px;display:inline-block" href=auth.clone() target="_blank">"Open the sign-in page ›"</a></div> })}
                            <div style="margin-top:16px">{checks.into_iter().map(|x| {
                                let n = api::n(&x, "n");
                                let (ic, cls) = icon(&x["ok"]);
                                let failed = x["ok"] == json!(false);
                                view! { <div class="row" style="gap:12px;align-items:flex-start;padding:7px 0;cursor:pointer" on:click=move |_| picked.set(n)>
                                    <span class=cls style="width:16px;font-weight:600">{ic}</span>
                                    <span class="grow"><div class:danger=failed>{s(&x, "title")}</div><div class="small cap">{s(&x, "detail")}</div></span></div> }
                            }).collect_view()}</div>
                            <div class="small cap" style="margin-top:10px">"Each check is one step of the MCP authorization spec, run by Stuntman in order."</div>
                        }.into_any()
                    }}
                </div>
                <div class="side">
                    {move || {
                        let r = run.get();
                        let c = cur.get();
                        let checks = r["checks"].as_array().cloned().unwrap_or_default();
                        let pick = picked.get();
                        let x = checks.iter().find(|x| api::n(x, "n") == pick).or_else(|| checks.iter().find(|x| x["ok"] == json!(false))).or_else(|| checks.iter().rev().find(|x| !x["ok"].is_null())).cloned();
                        let murl = s(&c, "metadataUrl");
                        view! {
                            {x.map(|x| { let failed = x["ok"] == json!(false); let n = api::n(&x, "n"); view! {
                                <div style="font-weight:600;font-size:15px" class:danger=failed>{s(&x, "title")}</div>
                                <div class="small" style="margin-top:6px">{if failed { why(n).to_string() } else { s(&x, "detail") }}</div>
                                <div class="caps" style="margin-top:22px">"What Stuntman sent and got"</div>
                                <pre class="yaml" style="white-space:pre-wrap;word-break:break-all;max-height:420px;overflow:auto">{format!("{}\n\n{}", s(&x, "sent"), s(&x, "got"))}</pre>
                            } })}
                            {(s(&c, "kind") == "metadata_url").then(|| { let (m2, m3) = (murl.clone(), murl.clone()); view! {
                                <div class="caps" style="margin-top:22px">"Client metadata Stuntman serves"</div>
                                <a class="link mono small" style="word-break:break-all" href=m3 target="_blank">{murl.clone()}</a>
                                <div class="small cap" style="margin-top:6px">"The app fetches it to learn the client's name and redirect URIs. Authorization servers accept only https URLs they can reach (no private addresses); a local Stuntman needs a public https address for this client."</div>
                                <button class="link" on:click=move |_| ui::copy(&m2, "Metadata URL")>"Copy"</button>
                            } })}
                        }
                    }}
                </div>
            </div>
            <Dialog open=add_open title="Add client">
                <div class="fd">
                    <div class="small t2">"A client Stuntman plays against the app's OAuth server."</div>
                    <div class="caps fd-l">"How it gets a client id"</div>
                    <div class="fd-cards">{[("dcr", "Registers itself", "dynamic client registration (RFC 7591)"), ("metadata_url", "Client id is a URL", "the app fetches the metadata document"), ("preregistered", "Pre-registered", "a client id and secret the app gave you")]
                        .into_iter().map(|(v, t, d)| view! { <button class="fd-card" class:on=move || a_kind.get() == v on:click=move |_| a_kind.set(v.into())><span class="fd-radio"></span><b>{t}</b><span class="small t2">{d}</span></button> }).collect_view()}</div>
                    <div class="row" style="gap:12px">
                        <div class="grow"><div class="caps fd-l">"Id"</div><input class="input mono" style="width:100%" placeholder="partner-client" prop:value=move || a_id.get() on:input=move |e| a_id.set(event_target_value(&e))/></div>
                        <div class="grow"><div class="caps fd-l">"Name it shows"</div><input class="input" style="width:100%" placeholder="Partner client" prop:value=move || a_name.get() on:input=move |e| a_name.set(event_target_value(&e))/></div>
                    </div>
                    {move || (a_kind.get() == "preregistered").then(|| view! {
                        <div class="row" style="gap:12px">
                            <div class="grow"><div class="caps fd-l">"Client id"</div><input class="input mono" style="width:100%" prop:value=move || a_cid.get() on:input=move |e| a_cid.set(event_target_value(&e))/></div>
                            <div class="grow"><div class="caps fd-l">"Client secret (test)"</div><input class="input mono" type="password" style="width:100%" prop:value=move || a_sec.get() on:input=move |e| a_sec.set(event_target_value(&e))/></div>
                        </div> })}
                    <div class="fd-foot"><span class="grow"></span><button class="btn" on:click=move |_| add_open.set(false)>"Cancel"</button><button class="btn primary" on:click=add>"Add client"</button></div>
                </div>
            </Dialog>
        </div>
    }
}
