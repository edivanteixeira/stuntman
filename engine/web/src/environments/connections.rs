//! E3 · Connections: what the app under test sets to reach an environment, by name (its base
//! domain) or by path prefix on one address; each value copies, or all of them as .env lines.

use leptos::prelude::*;
use leptos_router::hooks::use_params_map;
use serde_json::Value;

use super::label;
use crate::{
    api::{self, s},
    ui,
};

#[component]
pub fn Connections() -> impl IntoView {
    let p = use_params_map();
    let name = move || p.get().get("name").unwrap_or_default();
    let by = RwSignal::new("name".to_string());
    let base = RwSignal::new("http://stuntman.stuntman.svc".to_string());
    let data = RwSignal::new(Value::Null);
    let env = RwSignal::new(Value::Null);
    let err = RwSignal::new(String::new());
    Effect::new(move |_| {
        let (n, b, k) = (name(), base.get(), by.get());
        leptos::task::spawn_local(async move {
            let q = if k == "prefix" { format!("?base={}", api::enc(&b)) } else { String::new() };
            match api::get(&format!("/environments/{n}/connections{q}")).await {
                Ok(v) => { data.set(v); err.set(String::new()) }
                Err(e) => err.set(e),
            }
            if let Ok(v) = api::get(&format!("/environments/{n}")).await { env.set(v); }
        });
    });
    view! {
        <div class="page">
            <div class="head">
                <div class="top">
                    <h1><a class="crumb" href="/environments">"Environments"</a><span class="crumb" style="margin:0 12px">"/"</span>
                        <a class="crumb" href=move || format!("/environments/{}", name())>{move || label(&name())}</a><span class="crumb" style="margin:0 12px">"/"</span>"Connections"</h1>
                    <span class="grow"></span>
                    <button class="btn primary" on:click=move |_| ui::copy(&s(&data.get_untracked(), "env"), ".env lines")>"Copy as .env"</button>
                </div>
            </div>
            <div class="body">
                <div class="pane pad">
                    <div class="t2" style="margin-bottom:16px">"What the app under test sets to talk to this environment. Copy as .env gives every value in full."</div>
                    <div class="row" style="gap:14px;margin-bottom:22px">
                        <span>"Address"</span>
                        <ui::Seg options=vec![("name", "By name"), ("prefix", "By prefix")] value=by set=Callback::new(move |v: String| by.set(v))/>
                        {move || if by.get() == "prefix" { view! {
                            <input class="input mono" style="width:300px" prop:value=move || base.get() on:change=move |e| base.set(event_target_value(&e).trim().to_string())/> }.into_any()
                        } else { view! { <span class="mono small cap">{move || s(&env.get(), "baseDomain")}</span> }.into_any() }}
                    </div>
                    {move || (!err.get().is_empty()).then(|| view! { <div class="err">{err.get()}</div> })}
                    <table class="tbl">
                        <thead><tr><th>"Service"</th><th>"Name"</th><th>"What"</th><th>"Value"</th><th></th></tr></thead>
                        <tbody>
                            {move || {
                                let mut last = String::new();
                                data.get()["items"].as_array().cloned().unwrap_or_default().into_iter().map(|it| {
                                    let svc = s(&it, "service");
                                    let first = svc != last;
                                    last = svc.clone();
                                    let v = s(&it, "value");
                                    let (v2, w) = (v.clone(), s(&it, "what"));
                                    view! {
                                        <tr>
                                            <td style:font-weight=if first { "500" } else { "400" }>{if first { svc } else { String::new() }}</td>
                                            <td class="t2">{s(&it, "name")}</td>
                                            <td class="t2">{w.clone()}</td>
                                            <td class="mono small" style="word-break:break-all">{v}</td>
                                            <td><button class="link" on:click=move |_| ui::copy(&v2, &w)>"Copy"</button></td>
                                        </tr>
                                    }
                                }).collect_view()
                            }}
                        </tbody>
                    </table>
                    <div class="small cap" style="margin-top:18px;max-width:900px">"By name needs *.<base domain> pointed at Stuntman (a DNS zone or CoreDNS). By prefix needs nothing, but Azure Blob, Service Bus, Key Vault and SMTP find the environment by host name or login, so they keep the base domain. Identity always uses the public URL."</div>
                </div>
            </div>
        </div>
    }
}
