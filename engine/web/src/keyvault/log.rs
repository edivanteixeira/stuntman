//! K5 · Access log: every operation on the vault's items (24 h), filtered by identity, operation,
//! result and item; one entry's caller, request and trace; what is worth a look.

use leptos::prelude::*;
use serde_json::Value;

use super::{sel, Head};
use crate::{
    api::{self, enc, s},
    ui,
};

/// The values of a column with how many rows have each, most first ("acme-api-mi · 2,340").
fn opts(rows: &[Value], k: &str) -> Vec<(String, String)> {
    let mut m: std::collections::BTreeMap<String, i64> = Default::default();
    for r in rows { let v = s(r, k); if !v.is_empty() { *m.entry(v).or_default() += 1; } }
    let mut v: Vec<(String, i64)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v.into_iter().map(|(x, n)| (x.clone(), format!("{x}  ·  {}", api::thousands(n)))).collect()
}

/// K5 · Burst: the same identity, operation, item and answer within 1 s, five times or more, is one row with a count.
fn bursts(rows: Vec<Value>) -> Vec<(Value, Vec<Value>)> {
    let key = |e: &Value| (s(e, "identity"), s(e, "op"), s(e, "item"), api::n(e, "status"));
    let mut out: Vec<(Value, Vec<Value>)> = vec![];
    for e in rows {
        if let Some((first, rest)) = out.last_mut() {
            let last_at = rest.last().map(|x| api::n(x, "at")).unwrap_or(api::n(first, "at"));
            if key(first) == key(&e) && (last_at - api::n(&e, "at")).abs() <= 1000 { rest.push(e); continue; }
        }
        out.push((e, vec![]));
    }
    // Groups under five stay separate rows.
    out.into_iter().flat_map(|(f, rest)| if rest.len() + 1 >= 5 { vec![(f, rest)] } else { std::iter::once((f, vec![])).chain(rest.into_iter().map(|r| (r, vec![]))).collect() }).collect()
}

#[component]
pub fn Log() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let (vault, item) = sel();
    let (f_id, f_op, f_res, f_item) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let search = RwSignal::new(String::new());
    Effect::new(move |_| f_item.set(item.get()));
    let qm = leptos_router::hooks::use_query_map();
    Effect::new(move |_| if let Some(r) = qm.get().get("result") { f_res.set(r); });
    let allv = super::vaults();
    let open_groups = RwSignal::new(std::collections::HashSet::<i64>::new());
    let all = RwSignal::new(Vec::<Value>::new());
    let findings = RwSignal::new(Vec::<Value>::new());
    let picked = RwSignal::new(Value::Null);
    let paused = RwSignal::new(false);
    Effect::new(move |_| {
        live.tick.track();
        let v = vault.get();
        if v.is_empty() || paused.get() { return; }
        leptos::task::spawn_local(async move {
            if let Ok(x) = api::get(&format!("/keyvault/log?vault={}&since=24h&limit=2000", enc(&v))).await { all.set(x["entries"].as_array().cloned().unwrap_or_default()); }
            if let Ok(x) = api::get(&format!("/keyvault/patterns?vault={}", enc(&v))).await { findings.set(x["findings"].as_array().cloned().unwrap_or_default()); }
        });
    });
    let rows = Memo::new(move |_| {
        let (i, o, r, it, q) = (f_id.get(), f_op.get(), f_res.get(), f_item.get(), search.get().to_lowercase());
        all.get().into_iter().filter(|e| (i.is_empty() || s(e, "identity") == i) && (o.is_empty() || s(e, "op") == o) && (it.is_empty() || s(e, "item") == it) && (q.is_empty() || s(e, "item").contains(&q) || s(e, "identity").contains(&q))
            && (r.is_empty() || match r.as_str() { "ok" => api::n(e, "status") < 400, "refused" => api::n(e, "status") >= 400, "would_fail" => !s(e, "wouldFail").is_empty(), x => api::n(e, "status").to_string() == x })).collect::<Vec<_>>()
    });
    let pick = move |id: i64| leptos::task::spawn_local(async move { if let Ok(x) = api::get(&format!("/keyvault/log/{id}")).await { picked.set(x); } });
    let copy_lines = move |_| { let t = rows.get_untracked().iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"); ui::copy(&t, "JSON lines"); };
    let select = move |label: &'static str, sig: RwSignal<String>, choices: Vec<(String, String)>| view! {
        <label class="small" style="display:flex;flex-direction:column;gap:4px;margin-top:10px">{label}
            <select class="select small" style="margin-left:0;width:fit-content;max-width:100%" on:change=move |e| sig.set(event_target_value(&e))>
                <option value="" selected=move || sig.get().is_empty()>{format!("all  ·  {}", choices.len())}</option>
                {choices.iter().cloned().map(|(c, l)| { let (c2, c3) = (c.clone(), c.clone()); view! { <option value=c3 selected=move || sig.get() == c2>{l}</option> } }).collect_view()}
            </select></label>
    };
    view! {
        <div class="page">
            <Head active="log" search=search placeholder="Name, identity or tag…" right=move || view! {
                <button class="btn" on:click=move |_| paused.update(|p| *p = !*p)>{move || if paused.get() { "Live" } else { "Pause" }}</button>
                <button class="btn" on:click=copy_lines>"Copy as JSON lines"</button>
            }/>
            <div class="body">
                <div class="list narrow kvcol" style="padding:16px 20px;gap:4px">
                    <div class="caps">"Filter"</div>
                    {move || { let a = all.get(); view! {
                        {select("Identity", f_id, opts(&a, "identity"))}
                        {select("Operation", f_op, opts(&a, "op"))}
                        {select("Result", f_res, ["ok", "refused", "would_fail", "401", "403", "404", "429"].into_iter().map(|r| (r.to_string(), r.replace('_', " "))).collect())}
                        {select("Item", f_item, opts(&a, "item"))}
                    } }}
                    {move || { let f = findings.get(); (!f.is_empty()).then(|| view! {
                        <div class="caps" style="margin-top:22px">"Worth a look"</div>
                        {f.into_iter().map(|x| { let it = s(&x, "item"); view! {
                            <div class="callout attn-fill" style="margin:8px 0 0;padding:10px 12px;font-size:12px">{s(&x, "text")}
                                {(!it.is_empty()).then(|| { let it2 = it.clone(); view! { <div><button class="link" on:click=move |_| f_item.set(it2.clone())>"Show these ›"</button></div> } })}</div> } }).collect_view()}
                    }) }}
                </div>
                <div class="pane files">
                    <div class="ftable">
                        <div class="fhead caps"><span style="width:110px">"Time"</span><span style="width:170px">"Identity"</span><span class="grow">"Operation"</span><span style="width:60px">"Result"</span></div>
                        {move || {
                            let row = move |e: Value, burst: Option<(usize, i64)>, sub: bool| {
                                let id = api::n(&e, "id");
                                let st = api::n(&e, "status");
                                let wf = !s(&e, "wouldFail").is_empty();
                                let unknown = s(&e, "identity") == "unknown";
                                view! {
                                    <div class="frow" class:on=move || api::n(&picked.get(), "id") == id on:click=move |_| { pick(id); if burst.is_some() { open_groups.update(|g| if !g.insert(id) { g.remove(&id); }); } }>
                                        <span class="mono small cap" style="width:110px" style:padding-left=if sub { "14px" } else { "0" }>{api::hms(api::n(&e, "at"))}</span>
                                        <span class="mono" style="width:170px" class:danger=unknown>{s(&e, "identity")}</span>
                                        <span class="mono grow ell">{format!("{}  {}", s(&e, "op"), s(&e, "item"))}
                                            {burst.map(|(n, span)| view! { <span class="chip" style="margin-left:10px">{format!("{} in {} ms  {}", api::thousands(n as i64), span.max(1), if open_groups.get().contains(&id) { "▴" } else { "▾" })}</span> })}</span>
                                        <span class="mono" style="width:60px" class:danger={st >= 400} class:attn=wf>{if wf { format!("{st}*") } else { st.to_string() }}</span>
                                    </div>
                                }.into_any()
                            };
                            bursts(rows.get()).into_iter().take(500).flat_map(|(first, rest)| {
                                let id = api::n(&first, "id");
                                if rest.is_empty() { return vec![row(first, None, false)]; }
                                let span = api::n(&first, "at") - api::n(rest.last().unwrap(), "at");
                                let mut v = vec![row(first, Some((rest.len() + 1, span)), false)];
                                if open_groups.get().contains(&id) { v.extend(rest.into_iter().map(|e| row(e, None, true))); }
                                v
                            }).collect_view()
                        }}
                    </div>
                    {move || (all.get().is_empty() && !vault.get().is_empty()).then(|| { let url = super::vault_url(&allv.get(), &vault.get_untracked()); let u2 = url.clone(); view! {
                        <div class="callout" style="margin:20px 24px"><div class="t">"No operations in the last 24 h"</div>
                            <div class="t2">{format!("The app has not called the vault yet. Set its vault URL to {url}.")}</div>
                            <button class="link" on:click=move |_| ui::copy(&u2, "Vault URL")>"Copy vault URL"</button></div> } })}
                    <div class="note">{move || format!("Newest first. {} operation(s) shown; every one is kept 24 h. * allowed only because any token is on.", api::thousands(rows.get().len() as i64))}</div>
                </div>
                <div class="side">
                    {move || {
                        let e = picked.get();
                        if e.is_null() { return view! { <div class="small cap">"Pick an operation to see the caller, the request and the trace."</div> }.into_any(); }
                        let trace = s(&e, "traceId");
                        let wf = s(&e, "wouldFail");
                        let roles = e["roles"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(" + ")).unwrap_or_default();
                        view! {
                            <div class="row" style="gap:12px"><span class="mono" style="font-weight:600">{s(&e, "op")}</span><span class="mono">{s(&e, "item")}</span></div>
                            <div class="small cap">{format!("{} in {} ms  ·  version {}  ·  {}", api::n(&e, "status"), api::n(&e, "ms"), super::short(&s(&e, "version")), api::hms(api::n(&e, "at")))}</div>
                            <div class="caps" style="margin-top:20px">"Caller"</div>
                            <table class="kv">
                                <tr><td>"Identity"</td><td class="mono">{s(&e, "identity")}</td></tr>
                                <tr><td>"Object id"</td><td class="mono">{let o = s(&e, "oid"); if o.is_empty() { "—".into() } else { o }}</td></tr>
                                <tr><td>"Role"</td><td class="mono">{let r = s(&e, "role"); if !r.is_empty() { r } else if !roles.is_empty() { roles } else { "none".into() }}</td></tr>
                                <tr><td>"From"</td><td class="mono">{s(&e, "client")}</td></tr>
                            </table>
                            <div class="caps" style="margin-top:20px">"Request"</div>
                            <pre class="yaml" style="white-space:pre-wrap;word-break:break-all">{s(&e, "request")}</pre>
                            {(!trace.is_empty()).then(|| view! { <a class="link" href=format!("/telemetry/traces/{trace}")>"Open the trace ›"</a> })}
                            {(!s(&e, "why").is_empty()).then(|| view! { <div class="caps" style="margin-top:20px">"Answer"</div><div class="small" style="margin-top:6px">{s(&e, "why")}</div> })}
                            {(!wf.is_empty()).then(|| view! { <div class="callout attn-fill" style="margin-top:16px">{format!("A real vault would have refused this: {wf}")}</div> })}
                        }.into_any()
                    }}
                </div>
            </div>
        </div>
    }
}
