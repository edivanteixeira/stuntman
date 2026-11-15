//! The product-events tools (A13).

use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Tool};

use crate::{intake, plan, store};

fn expected_from(v: &Value) -> Result<Vec<plan::Expected>, ApiError> {
    match v {
        Value::Array(a) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => Ok(plan::Expected { event: s.clone(), ..Default::default() }),
                o => serde_json::from_value(o.clone()).map_err(|e| ApiError::bad(format!("events: {e}"))),
            })
            .collect(),
        _ => Err(ApiError::bad("events: a list of names or {event, props, min}")),
    }
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_product_events", "Product events the app sent (PostHog, Segment, Mixpanel, GA4), newest first, with the user, the properties and any personal data found in them.", |ctx, a| async move {
            let since = a.u64("since").map(|x| x as i64).unwrap_or(0);
            Ok(json!({"events": store::list(&ctx, a.str("source"), a.str("name"), a.str("user"), since, a.u64("limit").unwrap_or(200) as i64).await?}))
        })
        .param("source", "posthog | segment | mixpanel | ga4").param("name", "the event").param("user", "distinct id / user id").param("since", "ms").param("limit", "200")
        .route("GET", "/events"),
        Tool::new("event_people", "The users events arrived for, with counts.", |ctx, a| async move {
            Ok(json!({"people": store::people(&ctx, a.u64("since").map(|x| x as i64).unwrap_or(0)).await?}))
        })
        .param("since", "ms")
        .route("GET", "/events/people"),
        Tool::new("wait_for_event", "Waits until the app sends an event (by name; optionally a property value). Returns it.", |ctx, a| async move {
            let name = a.need("name")?;
            let since = util::now_ms() - a.ms("within")?.unwrap_or(0) as i64;
            let end = std::time::Instant::now() + std::time::Duration::from_millis(a.ms("timeout")?.unwrap_or(30_000));
            let want: Vec<(String, Value)> = a.get("props").and_then(|p| p.as_object()).map(|o| o.iter().map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
            loop {
                let got = store::list(&ctx, a.str("source"), Some(name.clone()), a.str("user"), since, 50).await?;
                if let Some(e) = got.into_iter().find(|e| want.iter().all(|(k, v)| &e["props"][k] == v)) {
                    return Ok(e);
                }
                if std::time::Instant::now() > end {
                    return Err(ApiError::new(408, format!("No “{name}” event yet")));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .req("name", "the event").param("source", "the collector").param("user", "the user").param("props", "{key: value} the event must have").param("timeout", "30s").param("within", "also events from this long ago")
        .route("GET", "/events/wait"),
        Tool::new("check_events", "Checks what the app sent against a plan (by name) or a list of events: each expected event, how many arrived and the properties some lacked. ok=false names the missing ones, so a test or CI run can fail on it.", |ctx, a| async move {
            let since = a.u64("since").map(|x| x as i64).or(a.ms("within")?.map(|w| util::now_ms() - w as i64)).unwrap_or(0);
            let expected = match (a.str("plan"), a.get("events")) {
                (_, Some(e)) => expected_from(e)?,
                (Some(p), None) => plan::load(&ctx).plans.into_iter().find(|x| x.name == p).map(|x| x.events).ok_or_else(|| ApiError::new(404, format!("No plan “{p}”")))?,
                (None, None) => return Err(ApiError::bad("plan or events")),
            };
            let arrived = store::list(&ctx, a.str("source"), None, a.str("user"), since, 10_000).await?;
            let mut r = plan::check(&expected, &arrived);
            r["plan"] = json!(a.str("plan"));
            r["since"] = json!(since);
            r["pii"] = json!(arrived.iter().filter(|e| e["pii"].as_array().map(|x| !x.is_empty()).unwrap_or(false)).map(|e| json!({"event": e["name"], "user": e["user"], "found": e["pii"]})).collect::<Vec<_>>());
            Ok(r)
        })
        .param("plan", "a plan's name").param("events", "or the list: names or {event, props, min}").param("since", "ms").param("within", "events from this long ago, 10m").param("source", "only one collector").param("user", "only one user")
        .route("POST", "/events/check"),
        Tool::new("list_event_plans", "The plans: which events (and properties) each flow must send.", |ctx, _a| async move {
            Ok(json!({"plans": plan::load(&ctx).plans, "failures": intake::FAILURES.iter().map(|f| json!({"kind": f.0, "label": f.1})).collect::<Vec<_>>()}))
        })
        .route("GET", "/events/plans"),
        Tool::new("set_event_plan", "Adds or replaces a plan: a name and its events (names, or {event, props, min}).", |ctx, a| async move {
            let name = a.need("name")?;
            let events = expected_from(a.get("events").ok_or_else(|| ApiError::bad("events"))?)?;
            let mut p = plan::load(&ctx);
            match p.plans.iter_mut().find(|x| x.name == name) {
                Some(x) => x.events = events,
                None => p.plans.push(plan::Plan { name: name.clone(), events }),
            }
            plan::save(&ctx, &p)?;
            ctx.live.send("events.plans", &name, "updated");
            Ok(json!({"plans": p.plans}))
        })
        .req("name", "e.g. signup").req("events", "[\"signup_completed\", {event: workspace_created, props: [plan]}]")
        .route("PUT", "/events/plans"),
        Tool::new("delete_event_plan", "Removes a plan.", |ctx, a| async move {
            let name = a.need("name")?;
            let mut p = plan::load(&ctx);
            p.plans.retain(|x| x.name != name);
            plan::save(&ctx, &p)?;
            Ok(json!({"plans": p.plans}))
        })
        .req("name", "the plan")
        .route("DELETE", "/events/plans/{name}"),
        Tool::new("force_events_failure", "The collector's next answers fail on purpose (count): 400, 413, 429, slow (3 s), drop_half (half the events lost, 200 anyway). kind=none clears.", |ctx, a| async move {
            let kind = a.need("kind")?;
            if kind != "none" && !intake::FAILURES.iter().any(|f| f.0 == kind) {
                return Err(ApiError::bad(format!("kind: {} | none", intake::FAILURES.iter().map(|f| f.0).collect::<Vec<_>>().join(" | "))));
            }
            let src = a.str("source").unwrap_or_default();
            if !src.is_empty() && !intake::SOURCES.contains(&src.as_str()) {
                return Err(ApiError::bad(format!("source: {}", intake::SOURCES.join(" | "))));
            }
            store::force(&ctx, &src, &kind, a.u64("count").unwrap_or(1) as i64, "you").await?;
            Ok(json!({"source": src, "kind": kind}))
        })
        .req("kind", "400 | 413 | 429 | slow | drop_half | none").param("source", "empty: any collector").param("count", "1")
        .route("POST", "/events/force"),
    ]
}
