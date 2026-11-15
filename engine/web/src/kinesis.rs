//! A21 · Kinesis: Data Streams on AWS_ENDPOINT_URL_KINESIS=http://localhost:4567. Streams with the
//! hash key space split by open shard, records (who put them), shards with their parents after a
//! reshard, enhanced fan-out consumers, putting a record as another producer would, provocations.

use leptos::prelude::*;
use leptos_router::hooks::use_query_map;
use serde_json::{json, Value};

use crate::{api::{self, enc, n, s}, ui::{self, AreaHead}};

/// "shardId-000000000003" → "…003".
fn short(id: &str) -> String {
    format!("…{}", &id[id.len().saturating_sub(3)..])
}

/// A sequence number people can tell apart: "4960…0412".
fn seq(x: &str) -> String {
    if x.len() < 10 { return x.to_string() }
    format!("{}…{}", &x[..4], &x[x.len() - 4..])
}

/// 2^128−1 and 2^127−1 are recognisable; the rest as Kinesis writes them.
fn hk(x: &str) -> String {
    match x { "340282366920938463463374607431768211455" => "2^128−1".into(), "170141183460469231731687303715884105727" => "2^127−1".into(), _ => x.to_string() }
}

#[component]
pub fn Kinesis() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let qm = use_query_map();
    let data = RwSignal::new(Value::Null);
    let recs = RwSignal::new(Vec::<Value>::new());
    let (body, pkey, ehk) = (RwSignal::new(String::new()), RwSignal::new(String::new()), RwSignal::new(String::new()));
    let (new_name, new_shards) = (RwSignal::new(String::new()), RwSignal::new(String::new()));
    let tab = Memo::new(move |_| qm.get().get("tab").unwrap_or("records".into()));
    let cur = Memo::new(move |_| qm.get().get("s").or_else(|| data.get()["streams"].as_array().and_then(|a| a.first().map(|x| s(x, "name")))).unwrap_or_default());
    Effect::new(move |_| {
        live.tick.track();
        let st = cur.get();
        leptos::task::spawn_local(async move {
            if let Ok(v) = api::get("/kinesis/streams").await { data.set(v) }
            if !st.is_empty() { if let Ok(v) = api::get(&format!("/kinesis/streams/{}/records?limit=50", enc(&st))).await { recs.set(v["records"].as_array().cloned().unwrap_or_default()) } }
        });
    });
    let stream = move || data.get()["streams"].as_array().and_then(|a| a.iter().find(|x| s(x, "name") == cur.get()).cloned()).unwrap_or(Value::Null);
    let put = move |_| {
        let st = cur.get_untracked();
        let mut b = json!({"data": body.get_untracked()});
        if !pkey.get_untracked().trim().is_empty() { b["partition_key"] = json!(pkey.get_untracked().trim()); }
        if !ehk.get_untracked().trim().is_empty() { b["explicit_hash_key"] = json!(ehk.get_untracked().trim()); }
        leptos::task::spawn_local(async move { match api::send("POST", &format!("/kinesis/streams/{}/records", enc(&st)), b).await {
            Ok(r) => ui::toast(format!("Put on {} · {}", s(&r, "shardId"), seq(&s(&r, "sequenceNumber")))), Err(e) => ui::toast_err(e) } });
    };
    let create = move |_| {
        let nm = new_name.get_untracked();
        let shards = new_shards.get_untracked().trim().parse::<u64>().unwrap_or(2);
        leptos::task::spawn_local(async move { match api::send("PUT", &format!("/kinesis/streams/{}", enc(nm.trim())), json!({"shards": shards})).await { Ok(_) => ui::toast("Stream created"), Err(e) => ui::toast_err(e) } });
    };
    let provoke = move |k: &'static str| {
        let st = cur.get_untracked();
        let b = match k { "reshard" => json!({"provoke": k, "stream": st}), "none" => json!({"provoke": k}), _ => json!({"provoke": k, "stream": st, "times": if k.starts_with("throughput") { 2 } else { 0 }}) };
        leptos::task::spawn_local(async move { match api::send("PUT", "/kinesis/config", b).await {
            Ok(_) => ui::toast(match k { "none" => "Back to normal", "reshard" => "Widest shard split", _ => "Provoked" }), Err(e) => ui::toast_err(e) } });
    };
    let provoked = move || s(&data.get()["provoke"], "kind");
    let link = move |st: &str, tab: &str| format!("/kinesis?s={}{}", enc(st), if tab == "records" { String::new() } else { format!("&tab={tab}") });
    view! {
        <div class="area">
            <div class="list" style="width:240px;background:var(--subtle)">
                <div style="padding:20px 20px 8px"><div class="strong" style="font-size:15px">"Kinesis"</div><div class="small cap">"Data Streams · AWS JSON 1.1"</div></div>
                {move || data.get()["streams"].as_array().cloned().unwrap_or_default().into_iter().map(|x| { let nm = s(&x, "name"); let n2 = nm.clone(); let open = n(&x, "openShards"); view! {
                    <a class="mrow" class:on=move || cur.get() == n2 href=link(&nm, "records")><div class="row"><span class="mono">{nm.clone()}</span><span class="grow"></span><span class="mono small cap">{api::thousands(n(&x, "records"))}</span></div>
                        <div class="small cap">{if s(&x, "mode") == "ON_DEMAND" { format!("on-demand · {open} shards") } else { format!("provisioned · {open} open shard{}", if open == 1 { "" } else { "s" }) }}</div></a> } }).collect_view()}
                <div class="stack" style="padding:12px 20px">
                    <input class="input mono" placeholder="new stream" prop:value=move || new_name.get() on:input=move |e| new_name.set(event_target_value(&e))/>
                    <input class="input mono" placeholder="shards  2" prop:value=move || new_shards.get() on:input=move |e| new_shards.set(event_target_value(&e))/>
                    <button class="btn small" on:click=create disabled=move || new_name.get().trim().is_empty()>"+ New stream"</button></div>
                <div class="small cap" style="padding:0 20px">"Streams appear when the app makes them, or here."</div>
            </div>
            <div class="page">
                {move || { let (st, tb) = (cur.get(), tab.get()); view! {
                    <AreaHead kind=Signal::derive(move || format!("Kinesis  ·  Data Streams  ·  AWS_ENDPOINT_URL_KINESIS={}  ·  us-east-1", s(&data.get(), "endpoint")))
                        title=Signal::derive(move || if cur.get().is_empty() { "Kinesis".into() } else { cur.get() })
                        tabs=vec![("records", "Records", link(&st, "records")), ("shards", "Shards", link(&st, "shards")), ("consumers", "Consumers", link(&st, "consumers"))]
                        counts=Signal::derive(move || { let x = stream(); vec![("records", api::thousands(n(&x, "records"))), ("shards", x["shards"].as_array().map(|a| a.len()).unwrap_or(0).to_string()),
                            ("consumers", x["consumers"].as_array().map(|a| a.len()).unwrap_or(0).to_string())] })
                        active=match tb.as_str() { "shards" => "shards", "consumers" => "consumers", _ => "records" }/> } }}
                <div class="body" style="flex-direction:column;overflow:auto;padding:16px 24px;gap:10px">
                    {move || { let x = stream(); (!x.is_null()).then(|| {
                        let shards = x["shards"].as_array().cloned().unwrap_or_default();
                        let closed = shards.iter().filter(|h| !h["open"].as_bool().unwrap_or(false)).count();
                        let tags = x["tags"].as_object().map(|o| o.iter().map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or_default())).collect::<Vec<_>>().join(", ")).unwrap_or_default();
                        let mut open: Vec<Value> = shards.into_iter().filter(|h| h["open"].as_bool().unwrap_or(false)).collect();
                        open.sort_by(|a, b| s(a, "startingHashKey").parse::<u128>().unwrap_or(0).cmp(&s(b, "startingHashKey").parse::<u128>().unwrap_or(0)));
                        view! {
                            <div class="small"><span class="caps">"Stream  "</span><span class="mono">{format!("{} · retention {} h · {} open shard{}{}{}", if s(&x, "mode") == "ON_DEMAND" { "on-demand" } else { "provisioned" }, n(&x, "retentionHours"),
                                open.len(), if open.len() == 1 { "" } else { "s" }, if closed > 0 { format!(", {closed} closed") } else { String::new() }, if tags.is_empty() { String::new() } else { format!(" · tags {tags}") })}</span></div>
                            <div class="caps">"Hash key space · MD5 of the partition key picks the shard"</div>
                            <div class="row" style="gap:2px;max-width:920px">{open.into_iter().enumerate().map(|(i, h)| view! {
                                <div class="mono small" title=format!("{} … {}", s(&h, "startingHashKey"), s(&h, "endingHashKey"))
                                    style=format!("flex:{} 1 0;min-width:0;display:flex;justify-content:space-between;gap:6px;padding:7px 8px;border:1px solid var(--border);border-radius:4px;background:{}", h["share"].as_f64().unwrap_or(1.0), if i % 2 == 1 { "var(--soft)" } else { "var(--surface)" })>
                                    <span style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap">{s(&h, "id")}</span><span class="cap">{api::thousands(n(&h, "records"))}</span></div> }).collect_view()}</div>
                            <div class="row mono small cap" style="max-width:920px"><span>"0"</span><span class="grow"></span><span>"2^128 − 1"</span></div> } }) }}
                    {move || match tab.get().as_str() {
                        "shards" => view! {
                            <table class="tbl"><tr><th>"Shard"</th><th>"Parent"</th><th>"Hash key range"</th><th>"Sequence range"</th><th>"State"</th><th>"Records"</th></tr>
                                {stream()["shards"].as_array().cloned().unwrap_or_default().into_iter().map(|h| { let open = h["open"].as_bool().unwrap_or(false); let parents = [s(&h, "parent"), s(&h, "adjacent")].into_iter().filter(|p| !p.is_empty()).map(|p| short(&p)).collect::<Vec<_>>().join(" + "); view! { <tr>
                                    <td class="mono">{s(&h, "id")}</td><td class="mono">{if parents.is_empty() { "—".into() } else { parents }}</td>
                                    <td class="mono small">{format!("{} … {}", hk(&s(&h, "startingHashKey")), hk(&s(&h, "endingHashKey")))}</td>
                                    <td class="mono">{format!("{} – {}", seq(&s(&h, "startingSequenceNumber")), seq(&s(&h, "endingSequenceNumber")))}</td>
                                    <td class=if open { "ok" } else { "cap" }>{if open { "open" } else { "closed" }}</td><td class="mono">{api::thousands(n(&h, "records"))}</td></tr> } }).collect_view()}</table>
                            <div class="small cap">"A split or merge closes the parent at its last sequence number; readers finish it, then read its children (ChildShards)."</div>
                        }.into_any(),
                        "consumers" => view! {
                            <table class="tbl"><tr><th>"Consumer"</th><th>"ARN"</th><th>"Status"</th><th>"Since"</th></tr>
                                {stream()["consumers"].as_array().cloned().unwrap_or_default().into_iter().map(|c| view! { <tr><td>{s(&c, "name")}</td><td class="mono small">{s(&c, "arn")}</td>
                                    <td class="ok">"ACTIVE"</td><td class="mono">{api::hms(n(&c, "createdAt"))}</td></tr> }).collect_view()}</table>
                            {stream()["consumers"].as_array().is_none_or(|a| a.is_empty()).then(|| view! { <div class="small cap">"No enhanced fan-out consumer: the app registers one with RegisterStreamConsumer, then reads with SubscribeToShard."</div> })}
                        }.into_any(),
                        _ => view! {
                            <table class="tbl"><tr><th>"Shard"</th><th>"Sequence"</th><th>"Partition key"</th><th>"Data"</th><th>"By"</th><th>"When"</th></tr>
                                {move || { let open: Vec<String> = stream()["shards"].as_array().cloned().unwrap_or_default().iter().filter(|h| h["open"].as_bool().unwrap_or(false)).map(|h| s(h, "id")).collect();
                                    recs.get().into_iter().map(|r| {
                                        let d = &r["data"]; let txt = if s(d, "format") == "binary" { format!("{} bytes", n(&r, "bytes")) } else { let v = &d["value"]; v.as_str().map(String::from).unwrap_or(v.to_string()) };
                                        let closed = !open.contains(&s(&r, "shard"));
                                        let by = s(&r, "producer"); let by = if by.starts_with("mcp:") || by.starts_with("api:") { "agent".to_string() } else { by };
                                        view! { <tr><td class="mono" title=s(&r, "shard")>{short(&s(&r, "shard"))}</td><td class="mono" title=s(&r, "sequenceNumber")>{seq(&s(&r, "sequenceNumber"))}</td>
                                            <td class="mono">{s(&r, "partitionKey")}</td><td class="mono small" class:cap=closed title=txt.clone()>{txt.chars().take(90).collect::<String>()}{closed.then_some("  · closed shard")}</td>
                                            <td>{by}</td><td class="mono">{api::hms(n(&r, "at"))}</td></tr> } }).collect_view() }}</table>
                            {move || recs.get().is_empty().then(|| view! { <div class="small cap">"No record yet."</div> })}
                            <div class="caps" style="margin-top:12px">"Put a record · as another producer would"</div>
                            <div class="stack" style="max-width:640px">
                                <textarea class="input mono" rows="2" placeholder="{\"shipment\": \"SHP-1043\", \"status\": \"DELIVERED\"}" prop:value=move || body.get() on:input=move |e| body.set(event_target_value(&e))></textarea>
                                <div class="grid2"><input class="input mono" placeholder="partition key  SHP-1043" prop:value=move || pkey.get() on:input=move |e| pkey.set(event_target_value(&e))/>
                                    <input class="input mono" placeholder="explicit hash key, optional" prop:value=move || ehk.get() on:input=move |e| ehk.set(event_target_value(&e))/></div>
                                <div><button class="btn primary small" on:click=put disabled=move || cur.get().is_empty() || body.get().trim().is_empty()>"Put record"</button></div></div>
                        }.into_any(),
                    }}
                    <div class="caps" style="margin-top:14px">"Provoke"</div>
                    <div class="row" style="gap:6px;flex-wrap:wrap">{[("throughput_puts", "Throughput (puts)"), ("throughput_gets", "Throughput (gets)"), ("not_found", "Not found"), ("expired_iterator", "Expired iterator"),
                        ("kms_throttling", "KMS throttling"), ("slow_puts", "Slow puts (3 s)"), ("partial_failures", "Partial failures"), ("reshard", "Reshard now"), ("none", "Normal")].into_iter().map(|(k, l)| view! {
                        <button class="btn small" class:primary=move || if k == "none" { provoked().is_empty() && !data.get().is_null() } else { provoked() == k } on:click=move |_| provoke(k) disabled=move || k == "reshard" && cur.get().is_empty()>{l}</button> }).collect_view()}</div>
                    {move || { let p = data.get()["provoke"].clone(); (!p.is_null()).then(|| { let label = config_label(&s(&p, "kind"));
                        view! { <div class="small attn">{format!("{}{}{}", if n(&p, "times") > 0 { format!("next {} call{}: ", n(&p, "times"), if n(&p, "times") == 1 { "" } else { "s" }) } else { String::new() },
                            label, if s(&p, "stream").is_empty() { String::new() } else { format!(" · only {}", s(&p, "stream")) })}</div> } }) }}
                    <div class="mono small cap">{move || format!("export AWS_ENDPOINT_URL_KINESIS={}   ·   any access key; region from the signature", s(&data.get(), "endpoint"))}</div>
                </div>
            </div>
        </div>
    }
}

fn config_label(k: &str) -> &'static str {
    match k {
        "throughput_puts" => "puts answer ProvisionedThroughputExceededException; the SDKs retry",
        "throughput_gets" => "GetRecords answers ProvisionedThroughputExceededException",
        "not_found" => "calls answer ResourceNotFoundException",
        "expired_iterator" => "GetRecords answers ExpiredIteratorException",
        "kms_throttling" => "puts and gets answer KMSThrottlingException",
        "slow_puts" => "each put waits before answering",
        "partial_failures" => "PutRecords fails every second record",
        _ => "provoked",
    }
}
