//! W3 · Recipients: the simulated person behind each number (or prefix, or everyone else), in match
//! order, and the steps every message to them goes through.

use leptos::prelude::*;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde_json::{json, Value};

use super::{pretty, Head};
use crate::{
    api::{self, enc, s},
    ui::{self, Confirm, Pop, Toggle},
};

fn summary(r: &Value) -> String {
    if r["whatsapp"] == false { return "not on WhatsApp  ·  131026".into(); }
    let mut p = vec![];
    for st in r["steps"].as_array().cloned().unwrap_or_default() {
        if let Some(v) = st["delivered_after"].as_str() { p.push(format!("delivered {v}")); }
        if let Some(v) = st["read_after"].as_str() { p.push(format!("read {v}")); }
        if st["never_read"] == true { p.push("read receipts off".into()); }
        if st["stay_at_sent"] == true { p.push("stays at sent".into()); }
        if let Some(c) = st["fail"].as_i64() { p.push(format!("fails {c}")); }
        if st["on_buttons"].is_object() { p.push(format!("taps “{}”", s(&st["on_buttons"], "tap"))); }
        if st["on_text"].is_object() { p.push("replies to texts".into()); }
        if st["send_image"].is_object() { p.push("sends an image".into()); }
        if st["send_location"].is_object() { p.push("sends a location".into()); }
        if let Some(n) = st["block_after"].as_i64() { p.push(format!("blocks after {n}")); }
    }
    if p.is_empty() { "delivered 1 s  ·  read 30 s".into() } else { p.join("  ·  ") }
}

const ADD: [(&str, &str); 10] = [("delivered_after", "Delivered after…"), ("read_after", "Read after…"), ("never_read", "Never read (receipts off)"), ("stay_at_sent", "Stay at sent (never delivered)"),
    ("fail", "Fail with a code…"), ("on_text", "Reply with a text…"), ("on_buttons", "Tap a button…"), ("send_image", "Send an image…"), ("send_location", "Send a location…"), ("block_after", "Block the business…")];

fn new_step(k: &str) -> Value {
    match k {
        "delivered_after" => json!({"delivered_after": "1s"}),
        "read_after" => json!({"read_after": "30s"}),
        "never_read" => json!({"never_read": true}),
        "stay_at_sent" => json!({"stay_at_sent": true}),
        "fail" => json!({"fail": 131049}),
        "on_text" => json!({"on_text": {"reply": "Yes, tomorrow morning", "after": "20s"}}),
        "on_buttons" => json!({"on_buttons": {"tap": "Talk to us", "after": "10s"}}),
        "send_image" => json!({"send_image": {"caption": "Here is the receipt", "after": "15s"}}),
        "send_location" => json!({"send_location": {"latitude": 40.7484, "longitude": -73.9857, "name": "Home", "after": "15s"}}),
        _ => json!({"block_after": 2}),
    }
}

#[component]
pub fn Recipients() -> impl IntoView {
    let live = expect_context::<api::Live>();
    let nav = use_navigate();
    let qm = use_query_map();
    let list = RwSignal::new(Vec::<Value>::new());
    let saved = RwSignal::new(Value::Null);
    let draft = RwSignal::new(Value::Null);
    let last = RwSignal::new(Vec::<Value>::new());
    let add_open = RwSignal::new(false);
    let rm_open = RwSignal::new(false);
    let yaml = RwSignal::new(String::new());
    let yaml_before = RwSignal::new(String::new());
    let creating = RwSignal::new(false);
    let pick = Memo::new(move |_| qm.get().get("number").unwrap_or_default());
    let load = move |keep: bool| leptos::task::spawn_local(async move {
        if let Ok(v) = api::get("/whatsapp/recipients").await {
            let l = v["recipients"].as_array().cloned().unwrap_or_default();
            list.set(l.clone());
            let y = super::templates::reqwasm_text("/api/whatsapp/files/recipients.yaml").await;
            if y != yaml.get_untracked() { yaml_before.set(yaml.get_untracked()); yaml.set(y); }
            if creating.get_untracked() || (keep && draft.get_untracked() != saved.get_untracked()) { return; }
            let p = pick.get_untracked();
            let x = l.iter().find(|x| s(&x["recipient"], "number") == p).or(l.first()).cloned().unwrap_or(Value::Null);
            saved.set(x["recipient"].clone()); draft.set(x["recipient"].clone()); last.set(x["last"].as_array().cloned().unwrap_or_default());
        }
    });
    Effect::new(move |_| { pick.track(); creating.set(false); load(false); });
    Effect::new(move |prev: Option<u64>| { let t = live.tick.get(); if prev.is_some() && s(&live.last.get_untracked(), "topic").starts_with("whatsapp.") { load(true); } t });
    let dirty = Memo::new(move |_| draft.get() != saved.get());
    let nav_save = nav.clone();
    let save = Callback::new(move |_: ()| {
        let d = draft.get_untracked();
        let nav = nav_save.clone();
        leptos::task::spawn_local(async move {
            match api::send("PUT", &format!("/whatsapp/recipients/{}", enc(&s(&d, "number"))), json!({"whatsapp": d["whatsapp"], "profile_name": s(&d, "profile_name"), "steps": d["steps"]})).await {
                Ok(_) => { ui::toast("Saved in whatsapp/recipients.yaml"); creating.set(false); saved.set(d.clone()); nav(&format!("/whatsapp/recipients?number={}", enc(&s(&d, "number"))), Default::default()); load(false); }
                Err(e) => ui::toast_err(e),
            }
        });
    });
    let remove = Callback::new(move |_| {
        let n = s(&saved.get_untracked(), "number");
        leptos::task::spawn_local(async move {
            match api::send("DELETE", &format!("/whatsapp/recipients/{}", enc(&n)), json!({})).await { Ok(_) => { ui::toast(format!("{n} removed")); saved.set(Value::Null); load(false); } Err(e) => ui::toast_err(e) }
        });
    });
    let set = move |i: usize, path: &'static str, v: Value| draft.update(|d| { if let Some(st) = d["steps"].get_mut(i) { if let Some(p) = st.pointer_mut(path) { *p = v; } } });
    let field = move |i: usize, path: &'static str, label: &'static str, w: &'static str| view! {
        <span class="small cap">{label}</span>
        <input class="input" style=format!("width:{w}") prop:value=move || draft.get()["steps"][i].pointer(path).map(|v| v.as_str().map(String::from).unwrap_or(v.to_string())).unwrap_or_default()
            on:input=move |e| { let t = event_target_value(&e); set(i, path, if path == "/fail" || path == "/block_after" { json!(t.parse::<i64>().unwrap_or(0)) } else { json!(t) }) }/>
    };
    let nav2 = nav.clone();
    view! {
        <div class="page">
            <Head active="recipients" right=move || view! {
                {move || dirty.get().then(|| view! { <span class="small attn">"Unsaved change"</span><button class="btn" on:click=move |_| { creating.set(false); draft.set(saved.get_untracked()) }>"Discard"</button> })}
                <button class="btn primary" disabled=move || !dirty.get() on:click=move |_| save.run(())>"Save"</button>
            }/>
            <div class="body">
                <div class="list" style="width:368px">
                    {move || list.get().into_iter().map(|x| {
                        let r = x["recipient"].clone();
                        let n = s(&r, "number");
                        let (n1, n2) = (n.clone(), n.clone());
                        let nav = nav.clone();
                        view! { <div class="mrow" class:on=move || !creating.get() && s(&saved.get(), "number") == n1 on:click=move |_| nav(&format!("/whatsapp/recipients?number={}", enc(&n2)), Default::default())>
                            <div class="row"><span class=if n == "*" { "" } else { "mono" }>{if n == "*" { "Everyone else".to_string() } else if n.contains('*') { n.clone() } else { pretty(&n) }}</span>
                                <span class="t2" style="margin-left:14px">{if n == "*" { "default".to_string() } else { s(&r, "profile_name") }}</span></div>
                            <div class=format!("small {}", if r["whatsapp"] == false { "danger" } else if r["steps"].to_string().contains("never_read") { "attn" } else { "cap" })>{summary(&r)}</div>
                        </div> }
                    }).collect_view()}
                    <button class="link add" style="display:block;padding:12px 24px" on:click=move |_| { creating.set(true); draft.set(json!({"number": "+1", "profile_name": "", "whatsapp": true, "steps": [{"delivered_after": "1s"}, {"read_after": "30s"}]})); }>"+ Add recipient"</button>
                    <div class="small cap" style="padding:12px 24px">"A recipient is the simulated person behind a number: whether WhatsApp reaches them, how fast they read, how they answer. The first match wins; “Everyone else” catches the rest."</div>
                </div>
                <div class="pane pad form"><div style="max-width:520px">
                    {move || {
                        let d = draft.get();
                        if d.is_null() { return view! { <div class="cap">"Loading…"</div> }.into_any(); }
                        let is_default = s(&d, "number") == "*";
                        let bad = !is_default && !s(&d, "number").starts_with('+');
                        view! {
                            <div class="field"><label>"Number"</label>
                                <input class="input mono" class:bad=bad disabled=move || !creating.get() prop:value=s(&d, "number") on:input=move |e| draft.update(|x| x["number"] = json!(event_target_value(&e)))/>
                                {bad.then(|| view! { <div class="err">"Use the number with its country code, e.g. +1 312 555 0110, or a prefix ending in *."</div> })}</div>
                            {(!is_default).then(|| view! {
                                <div class="field"><label>"Profile name"</label><input class="input" prop:value=s(&d, "profile_name") on:input=move |e| draft.update(|x| x["profile_name"] = json!(event_target_value(&e)))/>
                                    <div class="small cap">"sent to the app as contacts.profile.name"</div></div>
                            })}
                            {(!is_default).then(|| {
                                // Another (earlier or later) recipient that also matches this number.
                                let me = s(&d, "number");
                                let digits: String = me.chars().filter(|c| c.is_ascii_digit()).collect();
                                let other = list.get().into_iter().map(|x| s(&x["recipient"], "number")).find(|o| *o != me && *o != "*" && o.ends_with('*')
                                    && digits.starts_with(&o.trim_end_matches('*').chars().filter(|c| c.is_ascii_digit()).collect::<String>()));
                                other.map(|o| view! { <div class="callout attn-fill small" style="margin:8px 0">{format!("{me} is also matched by {o}. The one higher in the list wins.")}</div> })
                            })}
                            <div class="row" style="gap:10px;margin:8px 0 18px"><Toggle on=Signal::derive(move || draft.get()["whatsapp"] != false) set=move |v: bool| draft.update(|x| x["whatsapp"] = json!(v))/>
                                <span>"Has WhatsApp"</span><span class="small cap">"off: sends fail with 131026"</span></div>
                            <div class="caps">"What happens to each message"</div>
                            {d["steps"].as_array().cloned().unwrap_or_default().into_iter().enumerate().map(|(i, st)| {
                                let k = st.as_object().and_then(|o| o.keys().next().cloned()).unwrap_or_default();
                                let label = ADD.iter().find(|(x, _)| *x == k).map(|(_, l)| l.trim_end_matches('…').to_string()).unwrap_or(k.clone());
                                view! { <div class="wa-step">
                                    <div class="row"><span class="num">{i + 1}</span><span class="grow">{label}</span><button class="link plain" on:click=move |_| draft.update(|x| { if let Some(a) = x["steps"].as_array_mut() { a.remove(i); } })>"×"</button></div>
                                    <div class="row" style="gap:8px;margin-left:30px">{match k.as_str() {
                                        "delivered_after" => field(i, "/delivered_after", "after", "90px").into_any(),
                                        "read_after" => field(i, "/read_after", "after", "90px").into_any(),
                                        "fail" => field(i, "/fail", "Meta code", "100px").into_any(),
                                        "block_after" => field(i, "/block_after", "after this many messages", "60px").into_any(),
                                        "on_text" => view! { {field(i, "/on_text/reply", "reply", "200px")}{field(i, "/on_text/after", "after", "70px")} }.into_any(),
                                        "on_buttons" => view! { {field(i, "/on_buttons/tap", "tap", "140px")}{field(i, "/on_buttons/after", "after", "70px")} }.into_any(),
                                        "send_image" => view! { {field(i, "/send_image/caption", "caption", "180px")}{field(i, "/send_image/after", "after", "70px")} }.into_any(),
                                        "send_location" => view! { {field(i, "/send_location/name", "name", "120px")}{field(i, "/send_location/after", "after", "70px")} }.into_any(),
                                        _ => view! { <span></span> }.into_any(),
                                    }}</div>
                                </div> }
                            }).collect_view()}
                            <span class="anchor"><button class="link" on:click=move |_| add_open.set(true)>"+ Add step"</button>
                                <Pop open=add_open>
                                    <div class="mh caps">"Delivery"</div>
                                    {ADD[..5].iter().map(|(k, l)| { let k = *k; view! { <div class="mi" on:click=move |_| draft.update(|x| { if let Some(a) = x["steps"].as_array_mut() { a.push(new_step(k)); } })>{*l}</div> } }).collect_view()}
                                    <div class="rule"></div><div class="mh caps">"The person"</div>
                                    {ADD[5..].iter().map(|(k, l)| { let k = *k; view! { <div class="mi" on:click=move |_| draft.update(|x| { if let Some(a) = x["steps"].as_array_mut() { a.push(new_step(k)); } })>{*l}</div> } }).collect_view()}
                                </Pop></span>
                            <div class="small cap" style="margin-top:16px">"Steps run for every message the app sends to this number, in order. A reply opens the 24 h window, as a real reply would."</div>
                            {(!is_default && !creating.get()).then(|| view! { <button class="btn danger" style="margin-top:18px" on:click=move |_| rm_open.set(true)>"Remove recipient"</button> })}
                        }.into_any()
                    }}
                </div></div>
                <div class="side envside">
                    <div class="caps">"Last messages to this number"</div>
                    {move || last.get().into_iter().map(|m| view! { <div class="hrow"><span class="mono small cap">{api::hm(api::n(&m, "at"))}</span><div>
                        <div class="mono small">{if s(&m, "template").is_empty() { format!("{}: {}", s(&m, "kind"), s(&m, "text").chars().take(28).collect::<String>()) } else { s(&m, "template") }}</div>
                        <div class="small cap">{format!("{}{}", s(&m, "status"), m["error"].as_i64().map(|e| format!(" {e}")).unwrap_or_default())}</div></div></div> }).collect_view()}
                    {move || { let n = s(&saved.get(), "number"); (!n.contains('*') && !n.is_empty()).then(|| { let nav = nav2.clone(); view! { <button class="link" on:click=move |_| nav(&format!("/whatsapp?number={}", enc(&n.chars().filter(|c| c.is_ascii_digit()).collect::<String>())), Default::default())>"Open the conversation ›"</button> } }) }}
                    <div class="caps" style="margin-top:20px">"whatsapp/recipients.yaml"</div>
                    <super::YamlMarked text=yaml before=yaml_before/>
                </div>
            </div>
            <Confirm open=rm_open question=Signal::derive(move || format!("Remove {}?", s(&saved.get(), "number"))) detail=Signal::derive(|| "Its messages then follow the next match or “Everyone else”. Past conversations stay.".to_string()) action="Remove" on_yes=remove/>
        </div>
    }
}
