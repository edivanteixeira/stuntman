//! The AI models tools (A12): calls, rules, failures, spend and prices.

use serde_json::{json, Value};
use stuntman_core::{util, ApiError, Tool};

use crate::{config, model, store};

fn rule_from(a: &stuntman_core::tools::Args) -> Result<config::Rule, ApiError> {
    let mut r = config::Rule { name: a.need("name")?, ..Default::default() };
    let w = a.get("when").cloned().unwrap_or(json!({}));
    r.when = serde_json::from_value(w).map_err(|e| ApiError::bad(format!("when: {e}")))?;
    for (k, f) in [("provider", &mut r.when.provider), ("api", &mut r.when.api), ("model", &mut r.when.model), ("contains", &mut r.when.contains), ("system_contains", &mut r.when.system_contains), ("tool", &mut r.when.tool)] {
        if let Some(v) = a.str(k) {
            *f = v;
        }
    }
    if !r.when.provider.is_empty() && !model::PROVIDERS.contains(&r.when.provider.as_str()) {
        return Err(ApiError::bad(format!("provider: {}", model::PROVIDERS.join(" | "))));
    }
    let ans = a.get("answer").cloned().unwrap_or(json!({}));
    r.answer = match ans {
        Value::String(s) => config::Reply { text: s, ..Default::default() },
        v => serde_json::from_value(v).map_err(|e| ApiError::bad(format!("answer: {e}")))?,
    };
    if let Some(t) = a.str("text") {
        r.answer.text = t;
    }
    if let Some(t) = a.get("tool_call") {
        r.answer.tool_call = Some(serde_json::from_value(t.clone()).map_err(|e| ApiError::bad(format!("tool_call: {e}")))?);
    }
    if let Some(j) = a.get("json") {
        r.answer.json = Some(j.clone());
    }
    if let Some(n) = a.u64("tokens") {
        r.answer.tokens = Some(n);
    }
    if let Some(s) = a.str("stop") {
        r.answer.stop = s;
    }
    Ok(r)
}

pub fn all() -> Vec<Tool> {
    vec![
        Tool::new("list_ai_calls", "The calls the app made to the simulated models, newest first: provider, API, model, tokens in and out, cost, why (the rule, the default, a forced failure).", |ctx, a| async move {
            let since = a.u64("since").map(|x| x as i64).unwrap_or(0);
            Ok(json!({"calls": store::list(&ctx, a.str("provider"), since, a.u64("limit").unwrap_or(100) as i64).await?}))
        })
        .param("provider", "openai | azure-openai | anthropic | gemini | openrouter | deepseek").param("since", "ms").param("limit", "100")
        .route("GET", "/ai/calls"),
        Tool::new("get_ai_call", "One model call with the whole request and response.", |ctx, a| async move {
            store::get(&ctx, &a.need("id")?).await?.ok_or_else(|| ApiError::new(404, "No such call"))
        })
        .req("id", "the call id")
        .route("GET", "/ai/calls/{id}"),
        Tool::new("wait_for_ai_call", "Waits until the app calls a model (optionally a provider, and a prompt containing a text). Returns the call.", |ctx, a| async move {
            let since = util::now_ms() - a.ms("within")?.unwrap_or(0) as i64;
            let end = std::time::Instant::now() + std::time::Duration::from_millis(a.ms("timeout")?.unwrap_or(30_000));
            let want = a.str("contains").unwrap_or_default().to_lowercase();
            loop {
                let calls = store::list(&ctx, a.str("provider"), since, 50).await?;
                if let Some(c) = calls.into_iter().find(|c| want.is_empty() || c["prompt"].to_string().to_lowercase().contains(&want)) {
                    return Ok(c);
                }
                if std::time::Instant::now() > end {
                    return Err(ApiError::new(408, "No such model call yet"));
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        })
        .param("provider", "the provider").param("contains", "text in the prompt").param("timeout", "30s").param("within", "also calls from this long ago, 10s")
        .route("GET", "/ai/calls/wait"),
        Tool::new("list_ai_rules", "The rules that shape the simulated answers (first match wins), the default answer and the prices per million tokens.", |ctx, _a| async move {
            let c = config::load(&ctx);
            Ok(json!({"rules": c.rules, "defaultText": c.default_text, "prices": c.prices, "failures": model::FAILURES.iter().map(|f| json!({"kind": f.0, "label": f.1})).collect::<Vec<_>>(), "forced": store::forced(&ctx).await?}))
        })
        .route("GET", "/ai/rules"),
        Tool::new("set_ai_rule", "Adds or changes a rule (by name). when: provider, api, model (prefix*), contains (the last user message), system_contains, tool (offered). answer: text, tool_call {name, arguments} (the model calls the tool first; text comes after the tool's result), json, tokens, stop=length. position: where in the list (0 = first).", |ctx, a| async move {
            let r = rule_from(&a)?;
            let mut c = config::load(&ctx);
            let pos = c.rules.iter().position(|x| x.name == r.name);
            match (pos, a.u64("position")) {
                (Some(i), Some(p)) => { c.rules.remove(i); c.rules.insert((p as usize).min(c.rules.len()), r.clone()); }
                (Some(i), None) => c.rules[i] = r.clone(),
                (None, Some(p)) => c.rules.insert((p as usize).min(c.rules.len()), r.clone()),
                (None, None) => c.rules.push(r.clone()),
            }
            config::save(&ctx, &c)?;
            ctx.live.send("ai.rules", "", "updated");
            Ok(json!({"rule": r, "rules": c.rules.len()}))
        })
        .req("name", "the rule's name").param("when", "{provider, api, model, contains, system_contains, tool}").param("contains", "shortcut for when.contains").param("provider", "shortcut").param("model", "shortcut")
        .param("answer", "{text, tool_call, json, tokens, stop} or the text").param("text", "shortcut").param("tool_call", "{name, arguments}").param("json", "a JSON answer").param("tokens", "pad to about N tokens").param("stop", "length").param("position", "0 = first")
        .route("PUT", "/ai/rules"),
        Tool::new("delete_ai_rule", "Removes a rule.", |ctx, a| async move {
            let n = a.need("name")?;
            let mut c = config::load(&ctx);
            let before = c.rules.len();
            c.rules.retain(|r| r.name != n);
            if c.rules.len() == before {
                return Err(ApiError::new(404, format!("No rule “{n}”")));
            }
            config::save(&ctx, &c)?;
            ctx.live.send("ai.rules", "", "updated");
            Ok(json!({"deleted": n}))
        })
        .req("name", "the rule")
        .route("DELETE", "/ai/rules/{name}"),
        Tool::new("force_ai_failure", "The next calls (count) to a provider (or any) fail on purpose: 429 (rate limit), 500, 529 (overloaded), filter (content filtered), length (cut by max_tokens), bad_json (broken tool arguments), slow (40 ms per token), cut (the stream stops halfway). kind=none clears.", |ctx, a| async move {
            let kind = a.need("kind")?;
            if kind != "none" && !model::FAILURES.iter().any(|f| f.0 == kind) {
                return Err(ApiError::bad(format!("kind: {} | none", model::FAILURES.iter().map(|f| f.0).collect::<Vec<_>>().join(" | "))));
            }
            let p = a.str("provider").unwrap_or_default();
            if !p.is_empty() && !model::PROVIDERS.contains(&p.as_str()) {
                return Err(ApiError::bad(format!("provider: {}", model::PROVIDERS.join(" | "))));
            }
            store::force(&ctx, &p, &kind, a.u64("count").unwrap_or(1) as i64, &a.str("_by").unwrap_or("you".into())).await?;
            Ok(json!({"provider": p, "kind": kind, "forced": store::forced(&ctx).await?}))
        })
        .req("kind", "429 | 500 | 529 | filter | length | bad_json | slow | cut | none").param("provider", "empty: any provider").param("count", "1")
        .route("POST", "/ai/force"),
        Tool::new("ai_spend", "Calls, tokens in/out, cached tokens, cost and failures per provider since a time (default: today).", |ctx, a| async move {
            let day = util::now_ms() - util::now_ms() % 86_400_000;
            let since = a.u64("since").map(|x| x as i64).unwrap_or(day);
            let rows = store::spend(&ctx, since).await?;
            let total = rows.as_array().map(|r| r.iter().map(|x| x["cost"].as_f64().unwrap_or(0.0)).sum::<f64>()).unwrap_or(0.0);
            Ok(json!({"since": since, "providers": rows, "cost": total}))
        })
        .param("since", "ms (default: today, UTC)")
        .route("GET", "/ai/spend"),
        Tool::new("set_ai_price", "The price of a model (or a pattern with *) per million tokens, for the spend counter. default_text changes the answer when no rule matches.", |ctx, a| async move {
            let mut c = config::load(&ctx);
            if let Some(t) = a.str("default_text") {
                c.default_text = t;
            }
            if let Some(m) = a.str("model") {
                let p = c.prices.entry(m.clone()).or_default();
                let f = |k: &str| a.get(k).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
                if let Some(x) = f("in") { p.input = x; }
                if let Some(x) = f("out") { p.out = x; }
                if let Some(x) = f("cached") { p.cached = x; }
            }
            config::save(&ctx, &c)?;
            Ok(json!({"prices": c.prices, "defaultText": c.default_text}))
        })
        .param("model", "gpt-6.1 or claude-*").param("in", "USD per million input tokens").param("out", "output").param("cached", "cached input").param("default_text", "the answer when no rule matches")
        .route("PUT", "/ai/prices"),
    ]
}
