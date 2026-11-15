//! A12 · AI models: the calls the app made to the simulated model APIs (prompt, answer, tokens,
//! cost), the rules that shape the answers, failures provoked for the next calls, spend.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

const PROVIDERS: [(&str, &str, &str); 6] = [("openai", "OpenAI", "chat · responses · embeddings"), ("azure-openai", "Azure OpenAI", "deployments · v1"), ("anthropic", "Anthropic", "messages · streaming"),
    ("gemini", "Gemini", "generateContent"), ("openrouter", "OpenRouter", "OpenAI-compatible"), ("deepseek", "DeepSeek", "OpenAI-compatible")];

fn base(p: &str) -> String {
    let origin = web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    let b = format!("{origin}{}", api::url(""));
    let root = b.trim_end_matches('/');
    match p {
        "openai" => format!("{root}/openai/v1"),
        "anthropic" => format!("{root}/anthropic"),
        "gemini" => format!("{root}/gemini"),
        "azure-openai" => format!("{root}/azure-openai"),
        "openrouter" => format!("{root}/openrouter/api/v1"),
        _ => format!("{root}/deepseek"),
    }
}

#[component]
fn Area(active: &'static str, children: Children) -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let prov = Memo::new(move |_| qm.get().get("p").unwrap_or("openai".into()));
    let spend = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/ai/spend").await { spend.set(v) } });
    });
    let count = move |p: &str| spend.get()["providers"].as_array().and_then(|a| a.iter().find(|x| s(x, "provider") == p).map(|x| n(x, "calls"))).unwrap_or(0);
    let pq = move || format!("?p={}", prov.get());
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"AI models"</div>
                    <div class="small cap">{move || format!("{} providers · ${:.2} today", PROVIDERS.len(), spend.get()["cost"].as_f64().unwrap_or(0.0))}</div></div>
                {PROVIDERS.iter().map(|(k, t, sub)| { let k = *k; view! {
                    <a class="mrow" class:on=move || prov.get() == k href=format!("/ai{}?p={k}", if active == "calls" { "".to_string() } else { format!("/{active}") })>
                        <div class="row"><span>{*t}</span><span class="grow"></span><span class="mono small cap">{move || count(k).to_string()}</span></div>
                        <div class="small cap">{*sub}</div></a> } }).collect_view()}
                <div class="small cap" style="padding:16px 20px">"Point the SDK's base URL here. Any key is accepted; it is never sent anywhere."</div>
            </div>
            <div class="page">
                <AreaHead kind=Signal::derive(move || format!("AI models  ·  {}  ·  {}", PROVIDERS.iter().find(|x| x.0 == prov.get()).map(|x| x.1).unwrap_or(""), base(&prov.get())))
                    title=Signal::derive(move || PROVIDERS.iter().find(|x| x.0 == prov.get()).map(|x| x.1.to_string()).unwrap_or_default())
                    tabs=vec![("calls", "Calls", format!("/ai{}", pq())), ("rules", "Rules", format!("/ai/rules{}", pq())), ("spend", "Spend", format!("/ai/spend{}", pq()))]
                    counts=Signal::derive(move || vec![("calls", count(&prov.get()).to_string())]) active=active
                    right=move || { let b = base(&prov.get()); view! { <button class="btn small" on:click=move |_| ui::copy(&b, "base URL")>"Copy base URL"</button> } }/>
                {children()}
            </div>
        </div>
    }
}

/// A12 · Calls.
#[component]
pub fn Calls() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let prov = Memo::new(move |_| qm.get().get("p").unwrap_or("openai".into()));
    let list = RwSignal::new(Vec::<Value>::new());
    let sel = RwSignal::new(String::new());
    let detail = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        let p = prov.get();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/ai/calls?provider={}&limit=100", enc(&p))).await { list.set(v["calls"].as_array().cloned().unwrap_or_default()) } });
    });
    Effect::new(move |_| {
        let id = sel.get();
        let first = list.get().first().map(|c| s(c, "id")).unwrap_or_default();
        let id = if id.is_empty() { first } else { id };
        if id.is_empty() { detail.set(Value::Null); return }
        leptos::task::spawn_local(async move { if let Ok(v) = api::get(&format!("/ai/calls/{}", enc(&id))).await { detail.set(v) } });
    });
    view! {
        <Area active="calls">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px">
                <table class="tbl">
                    <tr><th>"When"</th><th>"API"</th><th>"Model"</th><th>"Tokens in → out"</th><th>"Why"</th><th>"Status"</th></tr>
                    {move || list.get().into_iter().map(|c| {
                        let id = s(&c, "id");
                        let i2 = id.clone();
                        let st = n(&c, "status");
                        view! { <tr class:on=move || sel.get() == i2 style="cursor:pointer" on:click=move |_| sel.set(id.clone())>
                            <td class="mono">{api::hms(n(&c, "at"))}</td><td>{s(&c, "api")}</td><td class="mono">{s(&c, "model")}</td>
                            <td class="mono">{format!("{} → {}", api::thousands(n(&c, "tokensIn")), api::thousands(n(&c, "tokensOut")))}</td><td>{s(&c, "why")}</td>
                            <td class={if st >= 400 { "danger mono" } else { "ok mono" }}>{st.to_string()}</td></tr> } }).collect_view()}
                </table>
                {move || list.get().is_empty().then(|| view! { <div class="callout" style="margin-top:16px"><div class="t">"No model calls yet"</div>
                    <div class="t2">{format!("Set the SDK's base URL to {} (any key). Calls show here with their prompt, answer, tokens and cost.", base(&prov.get()))}</div></div> })}
                {move || { let d = detail.get(); (!d.is_null()).then(|| { let p = d["prompt"].clone(); view! {
                    <div class="section" style="margin-top:20px">
                        <div class="caps">{format!("Call · {} · {}{} · {} ms", s(&d, "api"), s(&d, "model"), if d["stream"].as_bool().unwrap_or(false) { " · streamed" } else { "" }, n(&d, "ms"))}</div>
                        <div class="caps small" style="margin-top:12px">"The prompt"</div>
                        <pre class="yaml" style="white-space:pre-wrap">{format!("system: {}\nuser:   {}\ntools:  {}{}", s(&p, "system"), s(&p, "lastUser"), p["tools"].as_array().map(|t| t.iter().map(|x| x.as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join(", ")).unwrap_or_default(), if p["toolResult"].as_bool().unwrap_or(false) { "\n(after a tool result)" } else { "" })}</pre>
                        <div class="caps small" style="margin-top:12px">{format!("What Stuntman answered  ·  {}", s(&d, "why"))}</div>
                        <pre class="yaml" style="white-space:pre-wrap">{s(&d, "answer")}</pre>
                        <div class="mono small t2">{format!("in {}  ·  out {}  ·  cached {}  ·  ${:.4}", n(&d, "tokensIn"), n(&d, "tokensOut"), n(&d, "cached"), d["cost"].as_f64().unwrap_or(0.0))}</div>
                    </div> } }) }}
            </div>
        </Area>
    }
}

/// A12b · Rules and failures.
#[component]
pub fn Rules() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let prov = Memo::new(move |_| qm.get().get("p").unwrap_or("openai".into()));
    let data = RwSignal::new(Value::Null);
    let (name, contains, text, tool, args) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let count = RwSignal::new("1".to_string());
    let kind = RwSignal::new("429".to_string());
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move { if let Ok(v) = api::get("/ai/rules").await { data.set(v) } });
    });
    let save = move |_| {
        let mut body = json!({"name": name.get_untracked(), "contains": contains.get_untracked(), "text": text.get_untracked()});
        if !tool.get_untracked().is_empty() {
            body["tool_call"] = json!({"name": tool.get_untracked(), "arguments": serde_json::from_str::<Value>(&args.get_untracked()).unwrap_or(json!({}))});
        }
        leptos::task::spawn_local(async move { match api::send("PUT", "/ai/rules", body).await { Ok(_) => ui::toast("Rule saved"), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |_| {
        let body = json!({"provider": prov.get_untracked(), "kind": kind.get_untracked(), "count": count.get_untracked().parse::<u64>().unwrap_or(1)});
        leptos::task::spawn_local(async move { match api::send("POST", "/ai/force", body).await { Ok(_) => ui::toast("The next calls fail on purpose"), Err(e) => ui::toast_err(e) } });
    };
    view! {
        <Area active="rules">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                <div class="caps">"Rules · first match wins"</div>
                {move || data.get()["rules"].as_array().cloned().unwrap_or_default().into_iter().map(|r| {
                    let nm = s(&r, "name");
                    let n2 = nm.clone();
                    let w = &r["when"];
                    let when = ["provider", "api", "model", "contains", "system_contains", "tool"].iter().filter(|k| !s(w, k).is_empty()).map(|k| format!("{} {}", k.replace('_', " "), s(w, k))).collect::<Vec<_>>().join(" · ");
                    let a = &r["answer"];
                    let ans = match (&a["tool_call"], a.get("json")) { (t, _) if t.is_object() => format!("tool {} {}, then text: {}", s(t, "name"), t["arguments"], s(a, "text")), (_, Some(j)) => format!("JSON {j}"), _ => s(a, "text") };
                    view! { <div class="stepcard"><div class="row"><span class="mono strong">{nm}</span><span class="grow"></span>
                        <button class="link" on:click=move |_| { let n3 = n2.clone(); leptos::task::spawn_local(async move { let _ = api::send("DELETE", &format!("/ai/rules/{}", enc(&n3)), json!({})).await; }) }>"Remove"</button></div>
                        <div class="small">{format!("when {}", if when.is_empty() { "any call".into() } else { when })}</div><div class="small t2">{format!("→ {ans}")}</div></div> } }).collect_view()}
                <div class="stack" style="max-width:760px">
                    <div class="grid2"><input class="input" placeholder="name (refund)" prop:value=move || name.get() on:input=move |e| name.set(event_target_value(&e))/>
                        <input class="input" placeholder="the last user message contains…" prop:value=move || contains.get() on:input=move |e| contains.set(event_target_value(&e))/></div>
                    <input class="input" placeholder="answer text" prop:value=move || text.get() on:input=move |e| text.set(event_target_value(&e))/>
                    <div class="grid2"><input class="input" placeholder="call a tool first (optional): lookup_order" prop:value=move || tool.get() on:input=move |e| tool.set(event_target_value(&e))/>
                        <input class="input mono" placeholder="its arguments: {\"order_id\": \"A-1042\"}" prop:value=move || args.get() on:input=move |e| args.set(event_target_value(&e))/></div>
                    <div><button class="btn primary small" on:click=save>"Add the rule"</button></div>
                </div>
                <div class="caps" style="margin-top:18px">"Provoke on the next calls"</div>
                <div class="row" style="gap:8px;flex-wrap:wrap">
                    {move || data.get()["failures"].as_array().cloned().unwrap_or_default().into_iter().map(|f| { let k = s(&f, "kind"); let k2 = k.clone(); view! {
                        <div class="choice" class:on=move || kind.get() == k on:click=move |_| kind.set(k2.clone()) style="min-width:150px"><div class="strong">{s(&f, "kind")}</div><div class="small cap">{s(&f, "label")}</div></div> } }).collect_view()}
                </div>
                <div class="row" style="gap:8px"><input class="input" style="width:90px" prop:value=move || count.get() on:input=move |e| count.set(event_target_value(&e))/><span class="small cap">"next calls"</span>
                    <button class="btn primary small" on:click=provoke>"Provoke"</button>
                    <span class="small cap">{move || data.get()["forced"].as_array().map(|a| a.iter().map(|f| format!("{} × {} ({})", s(f, "kind"), n(f, "left"), if s(f, "provider").is_empty() { "any".into() } else { s(f, "provider") })).collect::<Vec<_>>().join(" · ")).unwrap_or_default()}</span></div>
            </div>
        </Area>
    }
}

/// Spend per provider and the prices it is computed from.
#[component]
pub fn Spend() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let sp = RwSignal::new(Value::Null);
    let rules = RwSignal::new(Value::Null);
    Effect::new(move |_| {
        live.tick.track();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/ai/spend").await { sp.set(v) }
            if let Ok(v) = api::get("/ai/rules").await { rules.set(v) }
        });
    });
    view! {
        <Area active="spend">
            <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px">
                <table class="tbl"><tr><th>"Provider"</th><th>"Calls"</th><th>"Tokens in"</th><th>"Tokens out"</th><th>"Cached"</th><th>"Failures"</th><th>"Cost"</th></tr>
                    {move || sp.get()["providers"].as_array().cloned().unwrap_or_default().into_iter().map(|p| view! { <tr><td>{s(&p, "provider")}</td><td class="mono">{n(&p, "calls").to_string()}</td>
                        <td class="mono">{api::thousands(n(&p, "tokensIn"))}</td><td class="mono">{api::thousands(n(&p, "tokensOut"))}</td><td class="mono">{api::thousands(n(&p, "cached"))}</td>
                        <td class="mono">{format!("{} · {} forced", n(&p, "failures"), n(&p, "forced"))}</td><td class="mono">{format!("${:.4}", p["cost"].as_f64().unwrap_or(0.0))}</td></tr> }).collect_view()}</table>
                <div class="caps" style="margin-top:20px">"Prices · USD per million tokens (Settings via set_ai_price)"</div>
                <table class="tbl"><tr><th>"Model"</th><th>"In"</th><th>"Out"</th><th>"Cached"</th></tr>
                    {move || rules.get()["prices"].as_object().cloned().unwrap_or_default().into_iter().map(|(m, p)| view! { <tr><td class="mono">{m}</td><td class="mono">{p["in"].to_string()}</td><td class="mono">{p["out"].to_string()}</td><td class="mono">{p["cached"].to_string()}</td></tr> }).collect_view()}</table>
            </div>
        </Area>
    }
}
