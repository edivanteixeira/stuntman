//! E1 · Notifications › E-mail: the live list, filters, grouping by kind, and the message pane
//! (extracted values, preview, attachments, held decisions, after-delivery actions). E1b–E1d.

use leptos::{html, prelude::*};
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};
use serde_json::{json, Value};
use wasm_bindgen::{closure::Closure, JsCast};

use crate::{
    api::{self, s},
    ui::{self, Confirm, Pop, Seg, Toggle},
};

const STATUSES: [&str; 9] = ["delivered", "opened", "clicked", "bounced", "deferred", "dropped", "held", "spamreport", "processed"];
const LANGS: [&str; 3] = ["pt-BR", "en", "es"];
const INBOXES: [&str; 3] = ["kept", "temporary", "catch-all"];

#[derive(Clone, Default, PartialEq)]
struct Filters {
    status: Vec<String>,
    lang: Vec<String>,
    inbox: Vec<String>,
    kind: String,
    has: Vec<String>,
    since: String,
}

impl Filters {
    fn query(&self) -> String {
        let mut q = String::new();
        for (k, v) in [("status", &self.status), ("lang", &self.lang), ("inbox", &self.inbox), ("has", &self.has)] {
            if !v.is_empty() {
                q += &format!("&{k}={}", api::enc(&v.join(",")));
            }
        }
        if !self.kind.is_empty() {
            q += &format!("&kind={}", api::enc(&self.kind));
        }
        if !self.since.is_empty() {
            q += &format!("&since={}", self.since);
        }
        q
    }
    fn count(&self) -> usize {
        self.status.len() + self.lang.len() + self.inbox.len() + self.has.len() + (!self.kind.is_empty()) as usize + (!self.since.is_empty()) as usize
    }
    fn chips(&self) -> Vec<(String, String)> {
        let mut v = vec![];
        for (k, l) in [("status", &self.status), ("lang", &self.lang), ("inbox", &self.inbox), ("has", &self.has)] {
            for x in l {
                v.push((k.to_string(), x.clone()));
            }
        }
        if !self.kind.is_empty() {
            v.push(("kind".into(), self.kind.clone()));
        }
        if !self.since.is_empty() {
            v.push(("since".into(), self.since.clone()));
        }
        v
    }
    fn remove(&mut self, k: &str, x: &str) {
        match k {
            "status" => self.status.retain(|y| y != x),
            "lang" => self.lang.retain(|y| y != x),
            "inbox" => self.inbox.retain(|y| y != x),
            "has" => self.has.retain(|y| y != x),
            "kind" => self.kind.clear(),
            _ => self.since.clear(),
        }
    }
}

fn toggle_in(v: &mut Vec<String>, x: &str) {
    if let Some(i) = v.iter().position(|y| y == x) {
        v.remove(i);
    } else {
        v.push(x.to_string());
    }
}

/// The line under the subject: "delivered · en · otp 482913".
fn meta_line(m: &Value) -> String {
    let mut parts = vec![];
    let st = s(m, "status");
    parts.push(match st.as_str() {
        "bounced" | "dropped" | "deferred" if m["provoked"] == true && s(m, "statusBy") == "rule" => format!("{st} by inbox rule"),
        _ => st,
    });
    if !s(m, "lang").is_empty() {
        parts.push(s(m, "lang"));
    }
    if let Some(o) = m["extracted"].as_object() {
        let mine: Vec<_> = o.iter().filter(|(_, v)| v["by"] == "yours").collect();
        if let Some((k, v)) = mine.first() {
            let val = v["value"].as_str().map(String::from).unwrap_or(v["value"].to_string());
            parts.push(if val.starts_with("http") { k.to_string() } else { format!("{k} {val}") });
        } else if let Some(c) = o.get("code") {
            parts.push(format!("code {}", c["value"].as_str().unwrap_or("")));
        }
    }
    let n = api::n(m, "attachmentCount");
    if n > 0 {
        parts.push(format!("{n} attachment{}", if n == 1 { "" } else { "s" }));
    }
    parts.join("  ·  ")
}

fn inbox_tag(m: &Value, inboxes: &[Value]) -> (String, &'static str) {
    match s(m, "inbox").as_str() {
        "temporary" => {
            let exp = inboxes.iter().find(|i| s(i, "address") == s(m, "to")).and_then(|i| i["expiresAt"].as_i64());
            match exp {
                Some(e) if e > api::now() as i64 => (format!("temporary · {}", api::span(e - api::now() as i64)), "tag temp"),
                Some(_) => ("temporary · expired".into(), "tag"),
                None => ("temporary".into(), "tag temp"),
            }
        }
        "kept" => ("kept".into(), "tag kept"),
        o => (o.to_string(), "tag"),
    }
}

#[component]
pub fn Messages() -> impl IntoView {
    let params = use_params_map();
    let nav = use_navigate();
    let live = expect_context::<api::Live>();
    let env = expect_context::<RwSignal<Value>>();
    let selected = Memo::new(move |_| params.get().get("id").unwrap_or_default());

    let filters = RwSignal::new(Filters::default());
    let q = RwSignal::new(use_query_map().get_untracked().get("to").unwrap_or_default());
    let group = RwSignal::new("none".to_string());
    let paused = RwSignal::new(false);
    let new_while_paused = RwSignal::new(0u32);
    let list = RwSignal::new(Vec::<Value>::new());
    let total = RwSignal::new(-1i64);
    let all_total = RwSignal::new(0i64);
    let groups = RwSignal::new(Vec::<Value>::new());
    let open_groups = RwSignal::new(Vec::<String>::new());
    let fresh = RwSignal::new(Vec::<String>::new());
    let inboxes = RwSignal::new(Vec::<Value>::new());
    let filters_open = RwSignal::new(false);
    let group_open = RwSignal::new(false);
    let svc = RwSignal::new(Value::Null);

    let svc_list = super::area::services();
    let sq = use_query_map();
    let svc_name = Memo::new(move |_| {
        let l = svc_list.get();
        sq.get().get("service").filter(|n| l.iter().any(|x| s(x, "name") == *n))
            .or_else(|| l.iter().find(|x| x["default"] == true).or(l.first()).map(|x| s(x, "name"))).unwrap_or_default()
    });
    let load = move || {
        let only = sq.get_untracked().get("service").map(|n| format!("&service={}", api::enc(&n))).unwrap_or_default();
        let query = format!("/messages?limit=200&group=kind{only}{}{}", filters.get_untracked().query(), {
            let t = q.get_untracked();
            if t.trim().is_empty() { String::new() } else { format!("&q={}", api::enc(t.trim())) }
        });
        leptos::task::spawn_local(async move {
            match api::get(&query).await {
                Ok(v) => {
                    let msgs = v["messages"].as_array().cloned().unwrap_or_default();
                    let old: Vec<String> = list.get_untracked().iter().map(|m| s(m, "id")).collect();
                    if !old.is_empty() {
                        let new: Vec<String> = msgs.iter().map(|m| s(m, "id")).filter(|id| !old.contains(id)).collect();
                        if !new.is_empty() {
                            fresh.set(new);
                        }
                    }
                    if let Ok(v) = api::get("/inboxes").await {
                        inboxes.set(v["inboxes"].as_array().cloned().unwrap_or_default());
                    }
                    list.set(msgs);
                    total.set(api::n(&v, "total"));
                    groups.set(v["groups"].as_array().cloned().unwrap_or_default());
                }
                Err(e) => ui::toast_err(e),
            }
            if let Ok(v) = api::get(&format!("/messages?limit=1{only}")).await {
                all_total.set(api::n(&v, "total"));
            }
        });
    };
    leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/services").await {
            svc.set(v["services"][0].clone());
        }
    });
    // Reload on filter, search and live changes (unless paused).
    Effect::new(move |_| {
        filters.track();
        q.track();
        load();
    });
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let last = live.last.get_untracked();
        if prev.is_some() && s(&last, "topic") == "email.message" {
            if paused.get_untracked() {
                if s(&last, "what") == "created" {
                    new_while_paused.update(|n| *n += 1);
                }
            } else {
                load();
            }
        }
        t
    });

    let envname = move || api::s(&env.get(), "env").to_uppercase();
    let nav_row = nav.clone();
    let row = move |m: Value| {
        let id = s(&m, "id");
        let id2 = id.clone();
        let id3 = id.clone();
        let nav = nav_row.clone();
        let (tag, tag_class) = inbox_tag(&m, &inboxes.get_untracked());
        let provoked = m["provoked"] == true;
        view! {
            <div class="mrow" class:on=move || selected.get() == id2 class:fresh=move || fresh.get().contains(&id3)
                on:click=move |_| nav(&format!("/notifications/{id}"), Default::default())>
                <div class="l1"><span>{s(&m, "to")}</span><span class="mono small cap">{api::hm(api::n(&m, "at"))}</span></div>
                <div class="l2">{s(&m, "subject")}</div>
                <div class="l3">
                    <span>{if provoked { "◆ " } else { "" }}{meta_line(&m)}</span>
                    <span class=tag_class>{tag}</span>
                </div>
            </div>
        }
    };
    let row = StoredValue::new(row);

    // Picking another service in the column reloads the list for it.
    Effect::new(move |prev: Option<Option<String>>| { let v = sq.get().get("service"); if prev.is_some() { load(); } v });
    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <div class="titled"><div class="kind">{move || format!("E-mail  ·  {}", api::s(&expect_context::<RwSignal<Value>>().get(), "env").to_uppercase())}</div>
                        <h1>{move || svc_name.get()}</h1></div>
                    <span class="grow"></span>
                    <div class="anchor">
                        <button class="btn" on:click=move |_| group_open.set(true)>{move || if group.get() == "kind" { "Group: kind" } else { "Group: none" }}<span class="small">"▾"</span></button>
                        <Pop open=group_open>
                            <div class="mi" on:click=move |_| group.set("none".into())><span>"None"</span>{move || (group.get() == "none").then_some("✓")}</div>
                            <div class="mi" on:click=move |_| group.set("kind".into())><span>"Kind"</span>{move || (group.get() == "kind").then_some("✓")}</div>
                            <div class="mh small cap" style="max-width:240px;white-space:normal">"A kind is the subject with names, documents and numbers masked, or the template id header when the app sends one."</div>
                        </Pop>
                    </div>
                    <FiltersButton filters=filters open=filters_open/>
                    <input class="input search" placeholder="To, subject, code…" prop:value=move || q.get() on:input=move |e| q.set(event_target_value(&e))/>
                    <div class="row" style="margin-left:8px">
                        <span class="dot" class:off=move || paused.get() || !live.connected.get()></span>
                        {move || {
                            if !live.connected.get() {
                                view! { <span class="t2">"Reconnecting…"</span> }.into_any()
                            } else if paused.get() {
                                let n = new_while_paused.get();
                                view! {
                                    <span class="t2">"Paused"</span>
                                    {(n > 0).then(|| view! { <span class="sep attn">{format!("{n} new")}</span> })}
                                    <span class="sep"></span>
                                    <button class="link" on:click=move |_| { paused.set(false); new_while_paused.set(0); load(); }>"Resume"</button>
                                }.into_any()
                            } else {
                                view! { <span style="font-weight:600">"Live"</span><span class="sep"></span><button class="link plain" style="font-size:13px;font-weight:600" on:click=move |_| paused.set(true)>"Pause"</button> }.into_any()
                            }
                        }}
                    </div>
                </div>
                <div class="tabs">
                    <div class="tab on">"Messages"<span class="n">{move || all_total.get()}</span></div>
                    <a class="tab" href=move || format!("/inboxes?service={}", api::enc(&svc_name.get()))>"Inboxes"</a>
                    <a class="tab" href=move || format!("/services/email/extractors?service={}", api::enc(&svc_name.get()))>"Extractors"</a>
                    <a class="tab" href=move || format!("/services/email/s/{}", api::enc(&svc_name.get()))>"Settings"</a>
                    <span class="grow"></span>
                    <div class="row small">
                        {move || filters.get().chips().into_iter().map(|(k, x)| {
                            let (k2, x2) = (k.clone(), x.clone());
                            view! { <span class="chip">{x}<button on:click=move |_| filters.update(|f| f.remove(&k2, &x2))>"×"</button></span> }
                        }).collect_view()}
                        {move || (filters.get().count() > 0).then(|| view! { <button class="link" on:click=move |_| filters.set(Filters::default())>"Clear all"</button> })}
                    </div>
                </div>
            </div>
            <div class="body">
                <div class="list">
                    {move || {
                        let msgs = list.get();
                        let searching = !q.get().trim().is_empty() || filters.get().count() > 0;
                        if total.get() == 0 && !searching {
                            let svc = svc.get();
                            return view! {
                                <div class="empty">
                                    <h3>{format!("No e-mail through {} in {} yet", svc_name.get(), envname())}</h3>
                                    <div>"Point your app at this service's SMTP or SendGrid address (Settings tab)."</div>
                                    <div class="mono small" style="margin-top:14px">{s(&svc, "smtpAddress")}</div>
                                    <div class="mono small">{s(&svc, "sendgridUrl")}</div>
                                </div>
                            }.into_any();
                        }
                        if msgs.is_empty() && searching {
                            let t = q.get();
                            return view! {
                                <div class="empty">
                                    <h3>{if t.trim().is_empty() { "No e-mail matches these filters".to_string() } else { format!("No e-mail matches “{}”", t.trim()) }}</h3>
                                    <div class="small">{format!("Search looks at address, subject and extracted values in {}.", envname())}</div>
                                    <div class="row" style="justify-content:center;margin-top:12px">
                                        <button class="link" on:click=move |_| q.set(String::new())>"Clear search"</button>
                                        <span class="sep"></span>
                                        <button class="link" on:click=move |_| filters.set(Filters::default())>"Clear filters"</button>
                                    </div>
                                </div>
                            }.into_any();
                        }
                        if group.get() == "kind" {
                            groups.get().into_iter().map(|g| {
                                let k = s(&g, "kind");
                                let (k2, k3) = (k.clone(), k.clone());
                                let items: Vec<Value> = msgs.iter().filter(|m| s(m, "kind") == k).cloned().collect();
                                view! {
                                    <div class="group-head" on:click=move |_| open_groups.update(|o| toggle_in(o, &k2))>
                                        <span>{move || if open_groups.get().contains(&k3) { "▾ " } else { "▸ " }}{k.clone()}</span>
                                        <span class="mono small cap">{api::n(&g, "count")}</span>
                                    </div>
                                    {move || open_groups.get().contains(&k).then(|| items.clone().into_iter().map(|m| row.with_value(|r| r(m))).collect_view())}
                                }
                            }).collect_view().into_any()
                        } else {
                            view! {
                                {msgs.into_iter().map(|m| row.with_value(|r| r(m))).collect_view()}
                                <div class="note">"Inbox:  kept  ·  temporary (expires)  ·  catch-all (no inbox declared)"</div>
                            }.into_any()
                        }
                    }}
                </div>
                <div class="pane pad">
                    {move || {
                        let id = selected.get();
                        if id.is_empty() {
                            view! { <div class="empty"><h3>"Pick a message"</h3><div>"Everything your app sent in this environment lands here, in the order it arrived."</div></div> }.into_any()
                        } else {
                            view! { <MessagePane id=id/> }.into_any()
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

#[component]
fn FiltersButton(filters: RwSignal<Filters>, open: RwSignal<bool>) -> impl IntoView {
    let draft = RwSignal::new(Filters::default());
    let preview = RwSignal::new(-1i64);
    let extractors = RwSignal::new(Vec::<String>::new());
    Effect::new(move |_| {
        if open.get() {
            draft.set(filters.get_untracked());
            leptos::task::spawn_local(async move {
                if let Ok(v) = api::get("/channels/email/extractors").await {
                    extractors.set(v["yours"].as_array().map(|a| a.iter().map(|e| s(&e["extractor"], "name")).collect()).unwrap_or_default());
                }
            });
        }
    });
    Effect::new(move |_| {
        let qy = draft.get().query();
        if !open.get_untracked() {
            return;
        }
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get(&format!("/messages?limit=1{qy}")).await {
                preview.set(api::n(&v, "total"));
            }
        });
    });
    let checks = move |key: &'static str, opts: Vec<String>| {
        opts.into_iter()
            .map(|o| {
                let (o2, o3) = (o.clone(), o.clone());
                let get = move |f: &Filters| match key {
                    "status" => f.status.contains(&o2),
                    "lang" => f.lang.contains(&o2),
                    "inbox" => f.inbox.contains(&o2),
                    _ => f.has.contains(&o2),
                };
                view! {
                    <label class="check" style="width:110px;margin:3px 0">
                        <input type="checkbox" prop:checked=move || draft.with(|f| get(f)) on:change=move |_| draft.update(|f| toggle_in(match key {
                            "status" => &mut f.status, "lang" => &mut f.lang, "inbox" => &mut f.inbox, _ => &mut f.has }, &o3))/>
                        {o}
                    </label>
                }
            })
            .collect_view()
    };
    view! {
        <div class="anchor">
            <button class="btn" on:click=move |_| open.set(true)>{move || { let n = filters.get().count(); if n > 0 { format!("Filters  ·  {n}") } else { "Filters".into() } }}<span class="small">"▾"</span></button>
            <Pop open=open keep=true right=true>
                <div style="padding:10px 12px;width:380px">
                    <div class="caps" style="margin-bottom:6px">"Status"</div>
                    <div class="row wrap" style="gap:0">{checks("status", STATUSES.iter().map(|x| x.to_string()).collect())}</div>
                    <div class="caps" style="margin:12px 0 6px">"Language"</div>
                    <div class="row wrap" style="gap:0">{checks("lang", LANGS.iter().map(|x| x.to_string()).collect())}</div>
                    <div class="caps" style="margin:12px 0 6px">"Inbox"</div>
                    <div class="row wrap" style="gap:0">{checks("inbox", INBOXES.iter().map(|x| x.to_string()).collect())}</div>
                    <div class="caps" style="margin:12px 0 6px">"Has an extracted value"</div>
                    <div class="row wrap" style="gap:0">{move || checks("has", { let mut v = vec!["code".to_string()]; v.extend(extractors.get()); v })}</div>
                    <div class="caps" style="margin:12px 0 6px">"Kind"</div>
                    <input class="input" style="width:100%" placeholder="Any kind" prop:value=move || draft.get().kind on:input=move |e| draft.update(|f| f.kind = event_target_value(&e))/>
                    <div class="caps" style="margin:12px 0 6px">"Received"</div>
                    <div class="row">
                        {[("", "any time"), ("1h", "last hour"), ("today", "today"), ("7d", "7 days")].into_iter().map(|(v, l)| view! {
                            <div class="radio" class:on=move || draft.get().since == v on:click=move |_| draft.update(|f| f.since = v.to_string())><span class="r"></span>{l}</div>
                        }).collect_view()}
                    </div>
                    <div class="row" style="margin-top:14px">
                        <button class="link" on:click=move |_| draft.set(Filters::default())>"Clear"</button>
                        <span class="grow"></span>
                        <span class="small cap">{move || { let n = preview.get(); if n < 0 { String::new() } else { format!("{n} message{}", if n == 1 { "" } else { "s" }) } }}</span>
                        <button class="btn primary small" on:click=move |_| { filters.set(draft.get_untracked()); open.set(false); }>"Apply"</button>
                    </div>
                </div>
            </Pop>
        </div>
    }
}

/// Turns a selection into a starting rule: literal context before it, then a capture that
/// generalizes digits and letters. "Use this code: 482 913" → `code:\s*(\d{3} \d{3})`.
fn rule_for(text: &str, sel: &str) -> String {
    let sel = sel.trim();
    let before = text.find(sel).map(|i| &text[..i]).unwrap_or("");
    let ctx: String = before.chars().rev().take(14).collect::<Vec<_>>().into_iter().rev().collect();
    let ctx = ctx.rsplit('\n').next().unwrap_or("").trim_start();
    // Start at a word, not mid-word ("this code:", not "se this code:"), unless the context is one word.
    let full = ctx.len() == before.trim_start().len();
    let ctx = match ctx.find(' ') { Some(i) if !full && i + 1 < ctx.len() => &ctx[i + 1..], _ => ctx };
    let esc = |s: &str| s.chars().map(|c| if "\\.+*?()|[]{}^$".contains(c) { format!("\\{c}") } else { c.to_string() }).collect::<String>();
    let mut body = String::new();
    let mut last: Option<(char, usize)> = None;
    let flush = |b: &mut String, l: &Option<(char, usize)>| {
        if let Some((k, n)) = l {
            let class = match k { 'd' => "\\d", 'w' => "\\w", _ => "" };
            if !class.is_empty() {
                *b += &if *n == 1 { class.to_string() } else { format!("{class}{{{n}}}") };
            }
        }
    };
    for c in sel.chars() {
        let k = if c.is_ascii_digit() { 'd' } else if c.is_alphabetic() { 'w' } else { 'x' };
        match (&mut last, k) {
            (Some((lk, n)), k2) if *lk == k2 && k2 != 'x' => *n += 1,
            _ => {
                flush(&mut body, &last);
                if k == 'x' {
                    body += &if c == ' ' { " ?".to_string() } else { esc(&c.to_string()) };
                    last = None;
                } else {
                    last = Some((k, 1));
                }
            }
        }
    }
    flush(&mut body, &last);
    let ctx = esc(ctx.trim_end());
    if ctx.is_empty() { format!("({body})") } else { format!("{ctx}\\s*({body})") }
}

#[component]
fn MessagePane(id: String) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let msg = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    let tab = RwSignal::new("preview".to_string());
    let device = RwSignal::new("desktop".to_string());
    let dark = RwSignal::new(false);
    let result = RwSignal::new(Value::Null);
    let busy = RwSignal::new(String::new());
    let menu = RwSignal::new(false);
    let links_open = RwSignal::new(false);
    let defer_open = RwSignal::new(false);
    let bounce_open = RwSignal::new(false);
    let confirm_delete = RwSignal::new(false);
    let csv = RwSignal::new((String::new(), String::new()));
    let pick = RwSignal::new(None::<(String, i32, i32)>);
    let idv = StoredValue::new(id.clone());

    let fetch = move || {
        let id = idv.get_value();
        leptos::task::spawn_local(async move {
            match api::get(&format!("/messages/{id}")).await {
                Ok(v) => {
                    msg.set(v);
                    err.set(String::new());
                }
                Err(e) => err.set(e),
            }
        });
    };
    fetch();
    Effect::new(move |prev: Option<u64>| {
        let t = live.tick.get();
        let last = live.last.get_untracked();
        if prev.is_some() && s(&last, "topic") == "email.message" && (s(&last, "id") == idv.get_value() || s(&last, "id") == "*") {
            fetch();
        }
        t
    });

    let act = move |body: Value, label: &'static str| {
        let id = idv.get_value();
        busy.set(label.to_string());
        leptos::task::spawn_local(async move {
            let r = api::send("POST", &format!("/messages/{id}/events"), body).await;
            busy.set(String::new());
            match r {
                Ok(v) => result.set(json!({"label": label, "r": v})),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let decide = move |action: &'static str, kind: Option<&'static str>| {
        let id = idv.get_value();
        leptos::task::spawn_local(async move {
            match api::send("POST", &format!("/messages/{id}/decide"), json!({"action": action, "kind": kind})).await {
                Ok(v) => result.set(json!({"label": match action { "deliver" => "Delivered", "defer" => "Deferred", "bounce" => "Bounce", _ => "Dropped" }, "r": v})),
                Err(e) => ui::toast_err(e),
            }
        });
    };
    let nav_del = nav.clone();
    let delete = move |_| {
        let id = idv.get_value();
        let nav = nav_del.clone();
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/messages/{id}"), json!({})).await {
                Ok(_) => {
                    ui::toast("Message deleted");
                    nav("/notifications", Default::default());
                }
                Err(e) => ui::toast_err(e),
            }
        });
    };

    // Select any text in the message to extract it (E1c): works in Preview and Text.
    let frame = NodeRef::<html::Iframe>::new();
    let on_frame_load = move |_| {
        let Some(f) = frame.get() else { return };
        let Some(doc) = f.content_document() else { return };
        let fw = f.content_window();
        let cb = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
            let sel = fw.as_ref().and_then(|w| w.get_selection().ok().flatten()).and_then(|s| s.to_string().as_string()).unwrap_or_default();
            let rect = frame.get().map(|f| f.get_bounding_client_rect());
            let (x, y) = rect.map(|r| (r.left() as i32 + e.client_x(), r.top() as i32 + e.client_y())).unwrap_or((e.client_x(), e.client_y()));
            pick.set((!sel.trim().is_empty() && sel.len() < 120).then(|| (sel, x, y)));
        });
        let _ = doc.add_event_listener_with_callback("mouseup", cb.as_ref().unchecked_ref());
        cb.forget();
    };
    let on_text_up = move |e: web_sys::MouseEvent| {
        let sel = window().get_selection().ok().flatten().and_then(|s| s.to_string().as_string()).unwrap_or_default();
        pick.set((!sel.trim().is_empty() && sel.len() < 120).then(|| (sel, e.client_x(), e.client_y())));
    };
    let nav_pick = nav.clone();
    let extract_pick = move |_| {
        let Some((sel, _, _)) = pick.get_untracked() else { return };
        let m = msg.get_untracked();
        let text = s(&m, "text");
        let rule = rule_for(&text, &sel);
        let strip = sel.trim().chars().all(|c| c.is_ascii_digit() || c == ' ' || c == '-');
        pick.set(None);
        nav_pick(&format!("/services/email/extractors?new=1&from=text&rule={}&msg={}&strip={strip}", api::enc(&rule), idv.get_value()), Default::default());
    };

    view! {
        {move || (!err.get().is_empty()).then(|| view! { <div class="empty"><h3>{err.get()}</h3><div>"It may have been deleted or cleared by a test."</div></div> })}
        {move || {
            let m = msg.get();
            if m.is_null() {
                return view! { <div class="cap">"Loading…"</div> }.into_any();
            }
            let status = s(&m, "status");
            let events_n = m["events"].as_array().map(|a| a.len()).unwrap_or(0);
            let atts: Vec<Value> = m["attachments"].as_array().cloned().unwrap_or_default().into_iter().filter(|a| a["inline"] != true).collect();
            let n_atts = atts.len();
            let links: Vec<Value> = m["links"].as_array().cloned().unwrap_or_default();
            let extracted = m["extracted"].as_object().cloned().unwrap_or_default();
            let html_body = s(&m, "html");
            let text_body = s(&m, "text");
            let srcdoc = if html_body.is_empty() { format!("<pre style='white-space:pre-wrap;font:13px/1.6 sans-serif;padding:24px'>{}</pre>", text_body.replace('&', "&amp;").replace('<', "&lt;")) } else { html_body.clone() };
            let trace = s(&m, "traceId");
            let named_links: Vec<(String, String)> = extracted.iter().filter(|(_, v)| v["value"].as_str().map(|x| x.starts_with("http")).unwrap_or(false)).map(|(k, v)| (k.clone(), v["value"].as_str().unwrap().to_string())).collect();
            let delivered_ev = m["events"].as_array().and_then(|a| a.iter().find(|e| e["type"] == "delivered").cloned());
            let m2 = m.clone();
            let mt = m.clone();
            view! {
                <div class="mhead">
                    <div>
                        <h2>{s(&m, "subject")}</h2>
                        <div class="meta">
                            {if s(&m, "fromName").is_empty() { s(&m, "from") } else { format!("{} <{}>", s(&m, "fromName"), s(&m, "from")) }}
                            "  →  "{s(&m, "to")}<span class="sep"></span>{api::hms(api::n(&m, "at"))}<span class="sep"></span>
                            {if s(&m, "via") == "smtp" { "via SMTP" } else { "via SendGrid API" }}
                        </div>
                        <div class="meta mono small" style="margin-top:4px">
                            "kind  “"{s(&m, "kind")}"”"
                            {(!s(&m, "lang").is_empty()).then(|| view! { <span class="sep"></span>{s(&m, "lang")} })}
                            {(!trace.is_empty()).then(|| view! { <span class="sep"></span><button class="link" style="font:inherit;color:var(--link)" title="Copies the trace id. Telemetry, where it opens, is not in this build." on:click={ let t = trace.clone(); move |_| ui::copy(&t, "Trace id") }>{format!("trace {} ↗", &trace[..8.min(trace.len())])}</button> })}
                            <span class="sep"></span>{s(&m, "id")}
                        </div>
                    </div>
                    <div class="anchor">
                        <button class="btn icon" on:click=move |_| menu.set(true)>"⋯"</button>
                        <Pop open=menu right=true>
                            <a class="mi" href=api::url(&format!("/api/messages/{}/raw", s(&m2, "id"))) download>"Download .eml"</a>
                            <div class="mi" on:click={ let id = s(&m2, "id"); move |_| ui::copy(&id, "Message id") }>"Copy message id"</div>
                            <div class="mi" on:click={ let j = serde_json::to_string_pretty(&m2).unwrap_or_default(); move |_| ui::copy(&j, "JSON") }>"Copy as JSON"</div>
                            <div class="rule"></div>
                            <div class="mi danger" on:click=move |_| confirm_delete.set(true)>"Delete message"</div>
                        </Pop>
                    </div>
                </div>

                <div class="extracted">
                    <div class="row">
                        <span class="caps">"Extracted"</span><span class="grow"></span>
                        <span class="small cap">"Select any text in the message to extract it"</span>
                        <a class="btn small" href="/services/email/extractors">"Extractors ›"</a>
                    </div>
                    <div class="ex-items">
                        {extracted.iter().filter(|(_, v)| v["same"] != true).map(|(k, v)| {
                            let val = v["value"].as_str().map(String::from).unwrap_or(v["value"].to_string());
                            let is_link = val.starts_with("http");
                            let shown = if is_link { format!("…{}", val.split("://").nth(1).map(|x| x.chars().rev().take(30).collect::<String>().chars().rev().collect::<String>()).unwrap_or(val.clone())) } else { val.clone() };
                            let val2 = val.clone();
                            let by = if v["by"] == "yours" { format!("your extractor “{k}”") } else { "built-in".to_string() };
                            view! {
                                <div class="ex-item">
                                    <div class="row"><span class="k">{k.clone()}</span><span class="v" class:sm=is_link>{shown}</span>
                                        <button class="link plain" style="margin-left:12px" on:click=move |_| ui::copy(&val2, "Value")>"Copy"</button></div>
                                    <div class="by">{by}</div>
                                </div>
                            }
                        }).collect_view()}
                        {(!links.is_empty()).then(|| view! {
                            <div class="ex-item">
                                <div class="row"><span class="k">"links"</span><span class="v sm">{format!("{}  ·  …{}", links.len(), links.first().map(|l| { let u = s(l, "url"); u.split("://").nth(1).unwrap_or(&u).to_string() }).unwrap_or_default())}</span></div>
                                <div class="by">"auto-detected"</div>
                            </div>
                        })}
                        {(n_atts > 0).then(|| view! {
                            <div class="ex-item">
                                <div class="row"><span class="k">"attachments"</span><span class="v sm">{format!("{n_atts}  ·  {}", api::kb(atts.iter().map(|a| api::n(a, "size")).sum()))}</span></div>
                                <div class="by">"name, type, size, hash"</div>
                            </div>
                        })}
                        {(extracted.is_empty() && links.is_empty() && n_atts == 0).then(|| view! { <span class="small cap">"Nothing extracted. Add an extractor for the value your test needs."</span> })}
                    </div>
                </div>

                <div class="ptabs">
                    {[("preview", "Preview".to_string()), ("text", "Text".to_string()), ("html", "HTML".to_string()), ("headers", "Headers".to_string()), ("events", format!("Events  {events_n}"))]
                        .into_iter().chain((n_atts > 0).then(|| ("attachments", format!("Attachments  {n_atts}"))))
                        .map(|(k, l)| view! { <div class="ptab" class:on=move || tab.get() == k on:click=move |_| tab.set(k.to_string())>{l}</div> }).collect_view()}
                    <span class="grow"></span>
                    {move || (tab.get() == "preview").then(|| view! {
                        <Seg options=vec![("desktop", "Desktop"), ("mobile", "Mobile")] value=device set=move |v| device.set(v)/>
                        <span class="small t2">"Dark"</span><Toggle on=dark set=move |v| dark.set(v)/>
                    })}
                </div>

                {move || { let m = &mt; match tab.get().as_str() {
                    "preview" => view! {
                        <div class="preview">
                            <iframe node_ref=frame sandbox="allow-same-origin" srcdoc=srcdoc.clone() class:mobile=move || device.get() == "mobile" class:dark=move || dark.get() on:load=on_frame_load></iframe>
                        </div>
                        {move || dark.get().then(|| view! { <div class="small cap" style="margin-top:6px">"Dark simulates the e-mail clients that invert colours, so problems show up before a customer sees them."</div> })}
                    }.into_any(),
                    "text" => view! { <pre class="text" on:mouseup=on_text_up>{if text_body.is_empty() { "(no text part)".to_string() } else { text_body.clone() }}</pre> }.into_any(),
                    "html" => view! { <pre class="text">{if html_body.is_empty() { "(no HTML part)".to_string() } else { html_body.clone() }}</pre> }.into_any(),
                    "headers" => view! {
                        <table class="kv">{m["headers"].as_array().cloned().unwrap_or_default().into_iter().map(|h| view! { <tr><td>{h[0].as_str().unwrap_or("").to_string()}</td><td class="mono">{h[1].as_str().unwrap_or("").to_string()}</td></tr> }).collect_view()}</table>
                    }.into_any(),
                    "events" => view! {
                        <table class="kv">{m["events"].as_array().cloned().unwrap_or_default().into_iter().map(|e| {
                            let r = &e["reply"];
                            let reply = if r.is_null() { "not sent: no events URL, or type not chosen".to_string() } else if !r["status"].is_null() { format!("your app replied {} in {} ms", r["status"], r["ms"]) } else { format!("no answer: {}", s(r, "error")) };
                            view! { <tr><td>{format!("{}  {}", api::hms(api::n(&e, "at")), s(&e, "type"))}</td><td>{format!("{}  ·  {}", s(&e, "detail"), s(&e, "by"))}<div class="small cap">{reply}</div></td></tr> }
                        }).collect_view()}</table>
                    }.into_any(),
                    _ => view! {
                        <div style="margin-top:12px">
                            {atts.iter().cloned().map(|a| {
                                let idx = api::n(&a, "index");
                                let mid = s(&m, "id");
                                let url = api::url(&format!("/api/messages/{mid}/attachments/{idx}"));
                                let ext = s(&a, "name").rsplit('.').next().unwrap_or("").to_uppercase();
                                let is_csv = s(&a, "contentType").contains("csv") || ext == "CSV";
                                let (url2, name2) = (url.clone(), s(&a, "name"));
                                view! {
                                    <div class="att">
                                        <div class="badge">{ext}</div>
                                        <div class="grow"><div style="font-weight:500">{s(&a, "name")}</div>
                                            <div class="small t2">{format!("{}  ·  {}  ·  sha256 {}…{}", s(&a, "contentType"), api::kb(api::n(&a, "size")), &s(&a, "sha256")[..4], &s(&a, "sha256")[60..])}</div></div>
                                        {if is_csv {
                                            view! { <button class="link" on:click=move |_| { let (u, n) = (url2.clone(), name2.clone()); leptos::task::spawn_local(async move { if let Ok(t) = api::text(&u).await { csv.set((n, t)); } }); }>"Preview"</button> }.into_any()
                                        } else {
                                            view! { <a class="link" href=url.clone() target="_blank">"Preview"</a> }.into_any()
                                        }}
                                        <a class="link" href=url download>"Download"</a>
                                    </div>
                                }
                            }).collect_view()}
                            {move || { let (name, t) = csv.get(); (!t.is_empty()).then(|| {
                                let rows: Vec<&str> = t.lines().collect();
                                view! {
                                    <div class="small t2" style="margin-top:14px">{format!("{name}  ·  first {} of {} rows", rows.len().min(6).saturating_sub(1), rows.len().saturating_sub(1))}</div>
                                    <table class="csv">{rows.iter().take(6).map(|r| view! { <tr>{r.split([',', ';']).map(|c| view! { <td>{c.to_string()}</td> }).collect_view()}</tr> }).collect_view()}</table>
                                }
                            }) }}
                            <div class="small cap" style="margin-top:12px">"Attachments stay with the message while it is kept. Above the size limit the provider refuses the send; that is simulated as dropped."</div>
                        </div>
                    }.into_any(),
                }}}

                {match status.as_str() {
                    "held" | "deferred" if status == "held" || m["events"].as_array().map(|a| a.iter().any(|e| s(e, "detail") == "until delivered by hand")).unwrap_or(false) => view! {
                        <div class="held">
                            <div class="caps attn">"Before delivery"</div>
                            <div style="margin:6px 0 10px">{format!("{} since {}  ·  {}", status, api::hms(api::n(&m, "statusAt").max(api::n(&m, "at"))), s(&m, "to"))}</div>
                            <div class="row">
                                <button class="btn primary" on:click=move |_| decide("deliver", None)>"Deliver"</button>
                                <div class="anchor"><button class="btn" on:click=move |_| defer_open.set(true)>"Defer ▾"</button>
                                    <Pop open=defer_open>
                                        <div class="mi" on:click=move |_| decide("defer", Some("1"))>"1 time"</div>
                                        <div class="mi" on:click=move |_| decide("defer", Some("3"))>"3 times"</div>
                                        <div class="mi" on:click=move |_| decide("defer", Some("until"))>"until I deliver"</div>
                                    </Pop></div>
                                <div class="anchor"><button class="btn" on:click=move |_| bounce_open.set(true)>"Bounce ▾"</button>
                                    <Pop open=bounce_open>
                                        <div class="mi" on:click=move |_| decide("bounce", Some("hard"))><span>"hard"</span><span class="sub">"550 unavailable"</span></div>
                                        <div class="mi" on:click=move |_| decide("bounce", Some("soft"))><span>"soft"</span><span class="sub">"452 mailbox full"</span></div>
                                    </Pop></div>
                                <button class="btn danger" on:click=move |_| decide("drop", None)>"Drop"</button>
                            </div>
                            <div class="small t2" style="margin-top:10px">{
                                let until = api::n(&m, "heldUntil");
                                if until > 0 { format!("Decide within {}, or the inbox's steps decide. ", api::span(until - api::now() as i64)) } else { String::new() }
                            }"The provider's event (delivered, deferred, bounce, dropped) goes to your app as soon as you choose."</div>
                        </div>
                    }.into_any(),
                    "bounced" | "dropped" => view! {
                        <div class="after">
                            <div class="row"><span style="font-weight:600">"Not delivered"</span>
                                <span class="t2 s12">{format!("{status}  ·  {}  ·  {}", s(&m, "reason"), if s(&m, "statusBy") == "rule" { "by the inbox rule".to_string() } else { format!("by {}", s(&m, "statusBy")) })}</span></div>
                            <div class="small cap" style="margin-top:6px">"Nobody can open or click a message that was not delivered."</div>
                        </div>
                    }.into_any(),
                    _ => {
                        let nl = named_links.clone();
                        let all_links = links.clone();
                        view! {
                            <div class="after">
                                <div class="row"><span style="font-weight:600">"After delivery"</span>
                                    <span class="t2 s12">{match &delivered_ev {
                                        Some(e) => format!("delivered at {}  ·  {}", api::hms(api::n(e, "at")), if s(e, "by") == "step" { "by the inbox's steps".to_string() } else { format!("by {}", s(e, "by")) }),
                                        None => format!("{status}  ·  not delivered yet"),
                                    }}</span></div>
                                <div class="btns">
                                    <button class="btn" disabled=move || !busy.get().is_empty() on:click=move |_| act(json!({"type": "open"}), "Open")>{move || if busy.get() == "Open" { "Sending…" } else { "Open" }}</button>
                                    <div class="anchor">
                                        <button class="btn" disabled=all_links.is_empty() on:click=move |_| links_open.set(true)>"Click link ▾"</button>
                                        <Pop open=links_open>
                                            <div class="mh caps">"Links in this e-mail"</div>
                                            {nl.iter().cloned().map(|(k, u)| { let k2 = k.clone(); view! { <div class="mi" on:click=move |_| act(json!({"type": "click", "link": k2.clone()}), "Click")><span class="mono">{k}</span><span class="sub">{u.chars().rev().take(34).collect::<String>().chars().rev().collect::<String>()}</span></div> } }).collect_view()}
                                            {all_links.iter().enumerate().filter(|(_, l)| !nl.iter().any(|(_, u)| *u == s(l, "url"))).map(|(i, l)| view! {
                                                <div class="mi" on:click=move |_| act(json!({"type": "click", "link": i.to_string()}), "Click")><span class="sub" style="color:var(--ink)">{s(l, "url")}</span></div>
                                            }).collect_view()}
                                            <div class="mh small cap" style="max-width:300px;white-space:normal">"Named links (from extractors) come first. Clicking sends a click event and opens the link once from the simulator."</div>
                                        </Pop>
                                    </div>
                                    <button class="btn" disabled=move || !busy.get().is_empty() on:click=move |_| act(json!({"type": "spamreport"}), "Spam report")>"Spam report"</button>
                                    <button class="btn" disabled=move || !busy.get().is_empty() on:click=move |_| act(json!({"type": "unsubscribe"}), "Unsubscribe")>"Unsubscribe"</button>
                                </div>
                                {move || {
                                    let r = result.get();
                                    if r.is_null() { return None; }
                                    let label = s(&r, "label");
                                    let rr = &r["r"];
                                    if !rr["skipped"].is_null() { return Some(view! { <div class="small attn">{s(rr, "skipped")}</div> }.into_any()); }
                                    let reply = &rr["reply"];
                                    Some(if rr["sent"] == false {
                                        view! { <div class="small t2">{format!("{label} recorded  ·  no events URL is set, so your app got nothing. Set one in the Settings tab.")}</div> }.into_any()
                                    } else if reply["status"].as_u64().map(|c| c < 300).unwrap_or(false) {
                                        view! { <div class="small ok">{format!("{label} sent  ·  your app replied {} in {} ms", reply["status"], reply["ms"])}</div> }.into_any()
                                    } else if !reply["status"].is_null() {
                                        view! { <div class="small danger">{format!("Your app replied {} to the {} event", reply["status"], label.to_lowercase())}<span class="sep"></span><span class="t2">{s(reply, "body").chars().take(120).collect::<String>()}</span></div> }.into_any()
                                    } else {
                                        view! { <div class="small danger">{format!("No answer from your app: {}", s(reply, "error"))}</div> }.into_any()
                                    })
                                }}
                                <div class="small cap">"Bounce, defer and drop happen before delivery: set them in the inbox rule, or hold delivery to decide by hand."</div>
                            </div>
                        }.into_any()
                    }
                }}
            }.into_any()
        }}
        {move || pick.get().map(|(sel, x, y)| view! {
            <div class="popup" style=format!("left:{}px;top:{}px", x, y + 14)>
                <span>{format!("“{}”", sel.trim().chars().take(30).collect::<String>())}</span>
                <button on:click=extract_pick.clone()>"Extract as…"</button>
                <button style="font-weight:400;opacity:.7" on:click=move |_| pick.set(None)>"×"</button>
            </div>
        })}
        <Confirm open=confirm_delete question="Delete this message?" detail="It disappears for everyone; tests waiting for it will not see it." action="Delete" on_yes=move |_| delete(())/>
    }
}

#[cfg(test)]
mod t {
    #[test]
    fn rule_from_selection() {
        assert_eq!(super::rule_for("Use this code: 482 913. It expires", "482 913"), r"this code:\s*(\d{3} ?\d{3})");
    }
}
