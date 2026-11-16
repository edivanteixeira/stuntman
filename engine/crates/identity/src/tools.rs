//! I1–I5 as tools: each is an MCP tool and a route under /api/identity (names from the frames'
//! "API and MCP" sheets).

use serde_json::{json, Map, Value};
use stuntman_core::{tools::Args, util, ApiError, Ctx, Tool};

use crate::{config::{self, App, Provider, User}, flow::{self, Who}, store, token};

fn by(a: &Args) -> String {
    a.str("_by").unwrap_or("you".into())
}

async fn provider(ctx: &Ctx, a: &Args) -> Result<Provider, ApiError> {
    let mut p = config::pick(ctx, a.str("provider"))?;
    token::ensure_keys(ctx, &mut p).await?;
    Ok(p)
}

fn changed(ctx: &Ctx, p: &Provider, what: &str) {
    ctx.live.send("identity.provider", &p.name, what);
}

fn user_json(p: &Provider, u: &User) -> Value {
    let mut v = serde_json::to_value(u).unwrap_or(json!({}));
    v["groupCount"] = json!(u.groups.len());
    v["otherTenant"] = json!(u.tenant != p.tenant && !u.guest);
    v
}

/// A sign-in with its tokens decoded.
fn decoded(mut s: Value) -> Value {
    if let Some(t) = s["tokens"].as_object().cloned() {
        let mut d = Map::new();
        for k in ["id_token", "access_token"] {
            if let Some((h, c)) = t.get(k).and_then(|x| x.as_str()).and_then(token::decode) { d.insert(k.into(), json!({"header": h, "claims": c})); }
        }
        s["decoded"] = Value::Object(d);
    }
    s
}

fn list_of(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::Array(a) => Some(a.iter().filter_map(|x| x.as_str().map(|s| s.trim().to_string())).filter(|s| !s.is_empty()).collect()),
        Value::String(s) => Some(s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()),
        _ => None,
    }
}

pub fn all() -> Vec<Tool> {
    vec![
        // ---------------------------------------------------------------- providers
        Tool::new("list_providers", "Identity providers in this environment: flavour, tenant, Authority, users, apps and sign-ins in the last hour.", |ctx, _a| async move {
            let counts = store::counts(&ctx, util::now_ms() - 3_600_000).await?;
            let mut out = vec![];
            for mut p in config::list(&ctx) {
                token::ensure_keys(&ctx, &mut p).await?;
                out.push(json!({"name": p.name, "flavour": p.flavour, "label": config::flavour_label(&p.flavour), "tenant": p.tenant, "authority": token::issuer(&ctx, &p),
                    "users": p.users.len(), "apps": p.apps.len(), "signInsLastHour": counts.get(&p.name).copied().unwrap_or(0),
                    "provoked": store::provoked(&ctx, &p.name).await?.map(|(a, u, n, b)| json!({"answer": a, "until": u, "count": n, "by": b}))}));
            }
            Ok(json!({"providers": out, "flavours": config::FLAVOURS.iter().map(|(k, l)| json!({"id": k, "label": l})).collect::<Vec<_>>()}))
        }).route("GET", "/identity/providers"),
        Tool::new("set_provider", "Creates an identity provider (or changes its flavour/tenant): entra-v2, google, okta or oidc. Users and apps are added with set_user and set_app.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let name = a.need("name")?;
            config::valid_name(&name)?;
            let created = config::get(&ctx, &name).is_err();
            let mut p = config::get(&ctx, &name).unwrap_or(Provider { name: name.clone(), environment: ctx.env.to_string(), ..Default::default() });
            if let Some(t) = a.str("tenant") { config::valid_name(&t)?; p.tenant = t; }
            if p.tenant.is_empty() { p.tenant = format!("{name}-tenant"); }
            if p.tenant_id.is_empty() { p.tenant_id = config::guid(&format!("tenant:{}", p.tenant)); }
            if let Some(f) = a.str("flavour") { if !config::FLAVOURS.iter().any(|(k, _)| *k == f) { return Err(ApiError::bad("flavour: entra-v2, google, okta or oidc")); } p.flavour = f; }
            if let Some(o) = a.get("other_tenants").and_then(list_of) { p.other_tenants = o; }
            if let Some(x) = a.bool("saml") { p.saml = if x { Some(p.saml.clone().unwrap_or_default()) } else { None }; }
            if let Some(sm) = p.saml.as_mut() {
                if let Some(x) = a.bool("saml_sign_response") { sm.sign_response = x; }
                if let Some(x) = a.bool("saml_sign_assertion") { sm.sign_assertion = x; }
                if let Some(x) = a.str("saml_name_id") { if !["email", "oid", "upn"].contains(&x.as_str()) { return Err(ApiError::bad("saml_name_id: email | oid | upn")); } sm.name_id = x; }
            }
            if let Some(x) = a.bool("ldap") { p.ldap = if x { Some(p.ldap.clone().unwrap_or_default()) } else { None }; }
            if let Some(l) = p.ldap.as_mut() {
                if let Some(x) = a.str("ldap_base_dn") { l.base_dn = x; }
                if let Some(x) = a.str("ldap_provoke") { if !["", "normal", "busy", "unavailable", "timeout"].contains(&x.as_str()) { return Err(ApiError::bad("ldap_provoke: normal | busy | unavailable | timeout")); } l.provoke = if x == "normal" { String::new() } else { x }; }
            }
            if let Some(x) = a.bool("password_grant") { p.password_grant = x; }
            config::save(&ctx, &p)?;
            token::ensure_keys(&ctx, &mut p).await?;
            changed(&ctx, &p, "provider");
            let b = token::base(&ctx, &p);
            Ok(json!({"created": created, "authority": token::issuer(&ctx, &p), "provider": p.name,
                "saml": p.saml.as_ref().map(|_| json!({"metadata": format!("{b}/saml/metadata"), "sso": format!("{b}/saml/sso"), "entityId": crate::saml::entity_id(&ctx, &p)})),
                "ldap": p.ldap.as_ref().map(|_| json!({"url": format!("ldap://{}:{}", ctx.app_host().0, std::env::var("STUNTMAN_LDAP_PORT").unwrap_or("3389".into())), "baseDn": p.base_dn(), "bindDn": format!("uid=<user>,ou=people,{}", p.base_dn())})),
                "passwordGrant": p.password_grant}))
        }).req("name", "provider id, e.g. entra").param("tenant", "contoso-stg").param("flavour", "entra-v2 | google | okta | oidc").param("other_tenants", "[fabrikam-stg]")
          .param("saml", "bool: SAML 2.0 IdP").param("saml_sign_response", "bool").param("saml_sign_assertion", "bool (default true)").param("saml_name_id", "email | oid | upn")
          .param("ldap", "bool: LDAP directory").param("ldap_base_dn", "dc=acme,dc=example").param("ldap_provoke", "normal | busy | unavailable | timeout").param("password_grant", "bool: grant_type=password").route("PUT", "/identity/providers/{name}"),
        Tool::new("remove_provider", "Removes an identity provider: its file (users, apps, tenant) and its sign-ins, keys and provocations. Apps pointed at its Authority get 404.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let name = a.need("name")?;
            let p = config::get(&ctx, &name)?;
            std::fs::remove_file(config::dir(&ctx).join(format!("{name}.yaml"))).map_err(|e| ApiError::internal(e.to_string()))?;
            let n = name.clone();
            let gone = ctx.db.call(move |c| {
                let mut total = 0;
                for t in ["id_signins", "id_keys", "id_state", "id_provoke"] {
                    total += c.execute(&format!("DELETE FROM {t} WHERE provider=?1"), [&n])?;
                }
                Ok(total)
            }).await?;
            changed(&ctx, &p, "removed");
            Ok(json!({"removed": name, "rows": gone}))
        }).req("name", "provider id").route("DELETE", "/identity/providers/{name}"),
        // ---------------------------------------------------------------- I1 sign-ins
        Tool::new("list_sign_ins", "Sign-ins and tokens issued: the sentence, the result or error code (AADSTS…), newest first.", |ctx, a| async move {
            let since = util::now_ms() - util::parse_ms(&a.str("since").unwrap_or("1h".into())).ok_or_else(|| ApiError::bad("since: 15m, 1h, 1d"))? as i64;
            let rows = store::sign_ins(&ctx, a.str("provider"), a.str("user"), a.str("result"), since, a.u64("limit").unwrap_or(200) as u32).await?;
            Ok(json!({"signIns": rows.into_iter().map(|mut r| { r.as_object_mut().map(|o| { o.remove("tokens"); }); r }).collect::<Vec<_>>()}))
        }).param("provider", "provider").param("user", "user id").param("result", "ok | failed | AADSTS50020 …").param("since", "1h").param("limit", "200").route("GET", "/identity/sign-ins"),
        Tool::new("get_sign_in", "One sign-in: steps, tokens issued (decoded) and the claims.", |ctx, a| async move {
            let id = a.need("id")?.parse::<i64>().map_err(|_| ApiError::bad("id is a number"))?;
            Ok(decoded(store::sign_in_by_id(&ctx, id).await?))
        }).req("id", "sign-in id").route("GET", "/identity/sign-ins/{id}"),
        Tool::new("sign_in_as", "Completes the login page waiting for this app (or starts one) as a user, through MFA and consent; returns the app's callback to follow.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let who = match a.get("claims").filter(|c| c.is_object()) { Some(c) => Who::Claims(c.clone()), None => Who::User(a.need("user")?) };
            flow::sign_in_as(&ctx, &mut p, who, a.str("app"), a.str("mfa_code"), a.bool("consent").unwrap_or(true), &by(&a)).await
        }).req("user", "name, id or email (or claims={email, name, groups, roles, tenant})").param("app", "client id").param("mfa_code", "default 482913").param("consent", "true")
          .param("claims", "typed claims for one sign-in").param("provider", "provider").route("POST", "/identity/{provider}/sign-in-as"),
        Tool::new("provoke_sign_in", "The next sign-ins: mfa_required, mfa_fails, consent_required, user_cancels, wrong_tenant, expired (tokens already expired), bad_signature, idp_down; normal stops it.", |ctx, a| async move {
            let p = provider(&ctx, &a).await?;
            let ans = a.need("answer")?;
            if !["normal", "mfa_required", "mfa_fails", "consent_required", "user_cancels", "wrong_tenant", "expired", "bad_signature", "wrong_audience", "idp_down"].contains(&ans.as_str()) {
                return Err(ApiError::bad("answer: normal, mfa_required, mfa_fails, consent_required, user_cancels, wrong_tenant, expired, bad_signature, wrong_audience (SAML) or idp_down"));
            }
            let until = match a.str("for") { Some(f) => Some(util::now_ms() + util::parse_ms(&f).ok_or_else(|| ApiError::bad("for: 10m, 1h"))? as i64), None => None };
            store::provoke(&ctx, &p.name, &ans, until, a.u64("count").map(|c| c as i64), &by(&a)).await?;
            Ok(json!({"answer": ans}))
        }).req("answer", "normal | mfa_required | mfa_fails | consent_required | user_cancels | wrong_tenant | expired | bad_signature | idp_down").param("count", "next N sign-ins").param("for", "10m")
          .param("provider", "provider").route("POST", "/identity/{provider}/provoke"),
        // ---------------------------------------------------------------- I2 pending pages
        Tool::new("pending_logins", "Login pages waiting for a person, with the app and the scopes.", |ctx, a| async move {
            Ok(json!({"pending": store::pending(&ctx, a.str("provider")).await?}))
        }).param("provider", "provider").route("GET", "/identity/pending"),
        Tool::new("cancel_login", "The person cancels a waiting login page: the app gets error=access_denied.", |ctx, a| async move {
            let id = a.need("id")?;
            let (_, _, _) = store::get(&ctx, "pending", &id).await?.ok_or_else(|| ApiError::not_found("No such login page"))?;
            let prov = store::pending(&ctx, None).await?.into_iter().find(|x| x["id"] == json!(id)).map(|x| x["provider"].as_str().unwrap_or("").to_string()).unwrap_or_default();
            let p = config::get(&ctx, &prov)?;
            Ok(match flow::cancel(&ctx, &p, &id, &by(&a)).await { flow::Next::Back { url, sign_in, .. } => json!({"callback": url, "sign_in": sign_in}), _ => json!({"cancelled": true}) })
        }).req("id", "pending page id").route("POST", "/identity/pending/{id}/cancel"),
        // ---------------------------------------------------------------- I3 users
        Tool::new("list_users", "Synthetic people of a provider: tenant, groups, roles, MFA, state, consents.", |ctx, a| async move {
            let p = provider(&ctx, &a).await?;
            Ok(json!({"provider": p.name, "tenant": p.tenant, "tenants": std::iter::once(p.tenant.clone()).chain(p.other_tenants.clone()).collect::<Vec<_>>(), "users": p.users.iter().map(|u| user_json(&p, u)).collect::<Vec<_>>()}))
        }).param("provider", "provider").route("GET", "/identity/{provider}/users"),
        Tool::new("set_user", "Creates or changes a user; the next tokens carry the claims. groups may be a number (210 makes synthetic groups: the overage path).", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let id = a.need("user")?;
            let existing = p.user(&id).cloned();
            let mut u = existing.clone().unwrap_or(User { id: id.to_lowercase().replace(' ', "."), tenant: p.tenant.clone(), enabled: true, ..Default::default() });
            if let Some(x) = a.str("name") { u.name = x; }
            if let Some(x) = a.str("email") { if existing.as_ref().map(|e| e.email != x).unwrap_or(true) { config::email_ok(&x, &ctx.state.file.read().unwrap().allowed_email_domains)?; } u.email = x; }
            if u.name.is_empty() { u.name = id.clone(); }
            if u.email.is_empty() { return Err(ApiError::bad("email: a synthetic address, e.g. emma.carter@contoso.example.com")); }
            if let Some(x) = a.str("tenant") { u.tenant = x; }
            if let Some(x) = a.bool("guest") { u.guest = x; }
            match a.get("groups") {
                Some(Value::Number(n)) => { u.groups = (1..=n.as_u64().unwrap_or(0)).map(|i| format!("Group {i:03}")).collect(); }
                Some(v) => if let Some(g) = list_of(v) { u.groups = g; },
                None => {}
            }
            if let Some(r) = a.get("roles").and_then(list_of) { u.roles = r; }
            if let Some(x) = a.bool("mfa") { u.mfa = x; }
            if let Some(x) = a.bool("phone_lost") { u.phone_lost = x; }
            if let Some(x) = a.bool("enabled") { u.enabled = x; }
            if let Some(x) = a.str("password") { u.password = x; }
            if let Some(x) = a.bool("locked") { u.locked = x; }
            if let Some(Value::Object(m)) = a.get("extra") { u.extra = m.clone(); }
            if u.oid.is_empty() { u.oid = config::guid(&format!("{}:{}", p.tenant, u.id)); }
            let created = existing.is_none();
            match p.users.iter_mut().find(|x| x.id == u.id) { Some(x) => *x = u.clone(), None => p.users.push(u.clone()) }
            config::save(&ctx, &p)?;
            if !u.enabled { store::drop_refresh(&ctx, &p.name, &u.id, None).await?; }
            changed(&ctx, &p, "user");
            Ok(json!({"created": created, "user": user_json(&p, &u)}))
        }).req("user", "id or name").param("name", "display name").param("email", "synthetic email").param("tenant", "home tenant").param("guest", "true/false").param("groups", "list, or a number")
          .param("roles", "list").param("mfa", "true/false").param("phone_lost", "true/false").param("enabled", "true/false").param("password", "for LDAP and the password grant (secret://name or a synthetic literal; empty: any but \"wrong\")").param("locked", "true/false").param("extra", "{employeeId: E-10442}").param("provider", "provider")
          .route("PUT", "/identity/{provider}/users/{user}"),
        Tool::new("remove_user", "Removes a user from the provider (their refresh tokens stop).", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let id = p.user(&a.need("user")?).map(|u| u.id.clone()).ok_or_else(|| ApiError::not_found("No such user"))?;
            p.users.retain(|u| u.id != id);
            config::save(&ctx, &p)?;
            store::drop_refresh(&ctx, &p.name, &id, None).await?;
            changed(&ctx, &p, "user");
            Ok(json!({"removed": id}))
        }).req("user", "id").param("provider", "provider").route("DELETE", "/identity/{provider}/users/{user}"),
        Tool::new("revoke_consent", "The user's consent to an app is revoked: the next sign-in asks again; refresh tokens stop.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let uid = p.user(&a.need("user")?).map(|u| u.id.clone()).ok_or_else(|| ApiError::not_found("No such user"))?;
            let app = a.need("app")?;
            let cid = p.app(&app).map(|x| x.client_id.clone()).unwrap_or(app);
            if let Some(u) = p.users.iter_mut().find(|u| u.id == uid) { u.consented.retain(|c| *c != cid); }
            config::save(&ctx, &p)?;
            let n = store::drop_refresh(&ctx, &p.name, &uid, Some(&cid)).await?;
            changed(&ctx, &p, "consent");
            Ok(json!({"revoked": cid, "refreshTokensStopped": n}))
        }).req("user", "user").req("app", "client id").param("provider", "provider").route("DELETE", "/identity/{provider}/users/{user}/consents/{app}"),
        Tool::new("set_user_state", "Disables or enables a user: sign-ins stop at the login page (AADSTS50057); refresh tokens are refused.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let uid = p.user(&a.need("user")?).map(|u| u.id.clone()).ok_or_else(|| ApiError::not_found("No such user"))?;
            let en = a.bool("enabled").ok_or_else(|| ApiError::bad("enabled: true or false"))?;
            if let Some(u) = p.users.iter_mut().find(|u| u.id == uid) { u.enabled = en; }
            config::save(&ctx, &p)?;
            if !en { store::drop_refresh(&ctx, &p.name, &uid, None).await?; }
            changed(&ctx, &p, "user");
            Ok(json!({"user": uid, "enabled": en}))
        }).req("user", "user").req("enabled", "true or false").param("provider", "provider").route("PUT", "/identity/{provider}/users/{user}/state"),
        // ---------------------------------------------------------------- I4 apps
        Tool::new("list_apps", "App registrations: client id, type, redirect URIs, credentials, permissions and the tokens each gets.", |ctx, a| async move {
            let p = provider(&ctx, &a).await?;
            let b = token::base(&ctx, &p);
            Ok(json!({"provider": p.name, "authority": token::issuer(&ctx, &p), "discovery": format!("{}/v2.0/.well-known/openid-configuration", b), "apps": p.apps.iter().map(|x| {
                let mut v = serde_json::to_value(x).unwrap_or(json!({}));
                v["consentedBy"] = json!(p.users.iter().filter(|u| u.consented.contains(&x.client_id)).count());
                v["secretSet"] = json!(!util::resolve_secret(&x.secret, &ctx.data_dir).is_empty());
                v
            }).collect::<Vec<_>>()}))
        }).param("provider", "provider").route("GET", "/identity/{provider}/apps"),
        Tool::new("set_app", "Registers or changes an app: type (web, spa, daemon, api), redirect_uris (exact match), secret (secret://name), any_secret, permissions, optional_claims, multi_tenant.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let cid = a.need("app")?;
            let existing = p.app(&cid).cloned();
            let mut x = existing.clone().unwrap_or(App { client_id: cid.clone(), name: cid.clone(), kind: "web".into(), permissions: vec!["openid".into(), "profile".into(), "email".into(), "offline_access".into()], ..Default::default() });
            if let Some(v) = a.str("name") { x.name = v; }
            if let Some(v) = a.str("type") { if !["web", "spa", "daemon", "api"].contains(&v.as_str()) { return Err(ApiError::bad("type: web, spa, daemon or api")); } x.kind = v; }
            if let Some(v) = a.get("redirect_uris").and_then(list_of) { x.redirect_uris = v; }
            if let Some(v) = a.str("secret") { x.secret = v; }
            if let Some(v) = a.bool("any_secret") { x.any_secret = v; }
            if let Some(v) = a.get("permissions").and_then(list_of) { x.permissions = v; }
            if let Some(v) = a.get("optional_claims").and_then(list_of) { x.optional_claims = v; }
            if let Some(v) = a.get("exposes").and_then(list_of) { x.exposes = v; }
            if let Some(v) = a.str("app_id_uri") { x.app_id_uri = v; }
            if let Some(v) = a.bool("multi_tenant") { x.multi_tenant = v; }
            if x.kind == "api" && x.app_id_uri.is_empty() { x.app_id_uri = format!("api://{}", x.client_id); }
            if x.secret.is_empty() && (x.kind == "web" || x.kind == "daemon") && existing.is_none() {
                // A synthetic secret in Secrets, so the app reads it the way it would in production.
                let name = format!("{}-client-secret", x.client_id);
                let _ = util::set_secret(&ctx.data_dir, &name, &format!("demo-{}", &util::uuid()[..13]));
                x.secret = format!("secret://{name}");
            }
            match p.apps.iter_mut().find(|y| y.client_id == x.client_id) { Some(y) => *y = x.clone(), None => p.apps.push(x.clone()) }
            config::save(&ctx, &p)?;
            changed(&ctx, &p, "app");
            Ok(json!({"created": existing.is_none(), "app": x}))
        }).req("app", "client id").param("name", "display name").param("type", "web | spa | daemon | api").param("redirect_uris", "list").param("secret", "secret://name")
          .param("any_secret", "true/false").param("permissions", "scopes").param("optional_claims", "groups, email…").param("exposes", "scopes (api)").param("app_id_uri", "api://acme-api")
          .param("multi_tenant", "true/false").param("provider", "provider").route("PUT", "/identity/{provider}/apps/{app}"),
        Tool::new("remove_app", "Removes an app registration: its sign-ins and token requests are refused.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let cid = p.app(&a.need("app")?).map(|x| x.client_id.clone()).ok_or_else(|| ApiError::not_found("No such app"))?;
            p.apps.retain(|x| x.client_id != cid);
            config::save(&ctx, &p)?;
            changed(&ctx, &p, "app");
            Ok(json!({"removed": cid}))
        }).req("app", "client id").param("provider", "provider").route("DELETE", "/identity/{provider}/apps/{app}"),
        Tool::new("grant_admin_consent", "Grants scopes for everyone, as an administrator would (the login page stops asking).", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let cid = p.app(&a.need("app")?).map(|x| x.client_id.clone()).ok_or_else(|| ApiError::not_found("No such app"))?;
            let scopes = a.get("scopes").and_then(list_of).filter(|s| !s.is_empty());
            if let Some(x) = p.apps.iter_mut().find(|x| x.client_id == cid) {
                let add = scopes.unwrap_or(x.permissions.iter().filter(|s| s.starts_with("api://")).cloned().collect());
                for s in add { if !x.admin_consent.contains(&s) { x.admin_consent.push(s); } }
            }
            config::save(&ctx, &p)?;
            changed(&ctx, &p, "app");
            Ok(json!({"app": cid, "adminConsent": p.app(&cid).map(|x| x.admin_consent.clone())}))
        }).req("app", "client id").param("scopes", "default: the app's API permissions").param("provider", "provider").route("POST", "/identity/{provider}/apps/{app}/admin-consent"),
        // ---------------------------------------------------------------- I5 tenant
        Tool::new("get_tenant", "Issuer, discovery, flavour, signing keys (current and next), lifetimes, clock skew and the provider's provocations.", |ctx, a| async move {
            let p = provider(&ctx, &a).await?;
            let b = token::base(&ctx, &p);
            Ok(json!({"provider": p.name, "flavour": p.flavour, "label": config::flavour_label(&p.flavour), "tenant": p.tenant, "tenantId": p.tenant_id, "otherTenants": p.other_tenants,
                "issuer": token::issuer(&ctx, &p), "discovery": format!("{b}/v2.0/.well-known/openid-configuration"), "jwks": format!("{b}/discovery/v2.0/keys"),
                "keys": {"current": p.keys.current, "currentSince": store::key_created(&ctx, &p.name, &p.keys.current).await?, "next": p.keys.next,
                    "rotatedAt": store::get(&ctx, "rotated", &p.name).await?.filter(|(d, ..)| d["kid"] == json!(p.keys.current)).map(|(_, at, _)| at)},
                "lifetimes": p.lifetimes, "provoke": p.provoke, "file": config::file_name(&p.name), "yaml": config::text(&ctx, &p.name),
                "flavours": config::FLAVOURS.iter().map(|(k, l)| json!({"id": k, "label": l})).collect::<Vec<_>>(),
                "saml": p.saml.as_ref().map(|sm| json!({"metadata": format!("{b}/saml/metadata"), "sso": format!("{b}/saml/sso"), "entityId": crate::saml::entity_id(&ctx, &p), "signAssertion": sm.sign_assertion, "signResponse": sm.sign_response, "nameId": sm.name_id})),
                "ldap": p.ldap.as_ref().map(|l| json!({"url": format!("ldap://{}:{}", ctx.app_host().0, std::env::var("STUNTMAN_LDAP_PORT").unwrap_or("3389".into())), "baseDn": p.base_dn(), "provoke": l.provoke})),
                "passwordGrant": p.password_grant, "tokenEndpoint": format!("{b}/oauth2/v2.0/token"),
                "botframework": {"openid": format!("{}/v1/.well-known/openidconfiguration", crate::botframework::base(&ctx)), "connector": format!("{}/connector/", crate::botframework::base(&ctx))}}))
        }).param("provider", "provider").route("GET", "/identity/{provider}/tenant"),
        Tool::new("set_tenant", "Changes flavour (entra-v2, google, okta, oidc), lifetimes (id, access, refresh), clock_skew and other tenants.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            if let Some(f) = a.str("flavour") { if !config::FLAVOURS.iter().any(|(k, _)| *k == f) { return Err(ApiError::bad("flavour: entra-v2, google, okta or oidc")); } p.flavour = f; }
            for (k, slot) in [("id_lifetime", 0), ("access_lifetime", 1), ("refresh_lifetime", 2), ("clock_skew", 3)] {
                if let Some(v) = a.str(k) {
                    if v != "0s" && util::parse_ms(&v).is_none() { return Err(ApiError::bad(format!("{k}: like 60m, 24h, 10m"))); }
                    match slot { 0 => p.lifetimes.id = v, 1 => p.lifetimes.access = v, 2 => p.lifetimes.refresh = v, _ => p.lifetimes.clock_skew = v }
                }
            }
            if let Some(o) = a.get("other_tenants").and_then(list_of) { p.other_tenants = o; }
            config::save(&ctx, &p)?;
            changed(&ctx, &p, "tenant");
            Ok(json!({"tenant": p.tenant, "flavour": p.flavour, "lifetimes": p.lifetimes}))
        }).param("flavour", "entra-v2 | google | okta | oidc").param("id_lifetime", "60m").param("access_lifetime", "60m").param("refresh_lifetime", "24h").param("clock_skew", "0s or +10m")
          .param("other_tenants", "list").param("provider", "provider").route("PUT", "/identity/{provider}/tenant"),
        Tool::new("rotate_signing_key", "The next key becomes current and a new next one is published: apps that cached the JWKS at start-up reject new tokens.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let old = p.keys.current.clone();
            token::rotate(&ctx, &mut p).await?;
            store::put(&ctx, "rotated", &p.name, &p.name, &json!({"kid": p.keys.current})).await?;
            changed(&ctx, &p, "keys");
            Ok(json!({"was": old, "current": p.keys.current, "next": p.keys.next}))
        }).param("provider", "provider").route("POST", "/identity/{provider}/keys/rotate"),
        Tool::new("provoke_idp", "The provider itself fails: discovery (500, slow, wrong_issuer), jwks (unavailable, kid_missing, wrong_key) or token (invalid_grant, 500, slow); normal stops it.", |ctx, a| async move {
            let _w = crate::WRITE.lock().await;
            let mut p = provider(&ctx, &a).await?;
            let part = a.need("part")?;
            let ans = a.need("answer")?;
            let ok = match part.as_str() { "discovery" => ["normal", "500", "slow", "wrong_issuer"].contains(&ans.as_str()), "jwks" => ["normal", "unavailable", "kid_missing", "wrong_key"].contains(&ans.as_str()),
                "token" => ["normal", "invalid_grant", "500", "slow"].contains(&ans.as_str()), _ => return Err(ApiError::bad("part: discovery, jwks or token")) };
            if !ok { return Err(ApiError::bad("answer: discovery 500|slow|wrong_issuer, jwks unavailable|kid_missing|wrong_key, token invalid_grant|500|slow, or normal")); }
            match part.as_str() { "discovery" => p.provoke.discovery = ans.clone(), "jwks" => p.provoke.jwks = ans.clone(), _ => p.provoke.token = ans.clone() }
            config::save(&ctx, &p)?;
            changed(&ctx, &p, "provoke");
            if let Some(f) = a.str("for").filter(|_| ans != "normal") {
                let ms = util::parse_ms(&f).ok_or_else(|| ApiError::bad("for: 10m, 1h"))?;
                let (ctx2, name, part2, ans2) = (ctx.clone(), p.name.clone(), part.clone(), ans.clone());
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                    if let Ok(mut p) = config::get(&ctx2, &name) {
                        let slot = match part2.as_str() { "discovery" => &mut p.provoke.discovery, "jwks" => &mut p.provoke.jwks, _ => &mut p.provoke.token };
                        if *slot == ans2 { *slot = "normal".into(); let _ = config::save(&ctx2, &p); ctx2.live.send("identity.provider", &name, "provoke"); }
                    }
                });
            }
            Ok(json!({"part": part, "answer": ans}))
        }).req("part", "discovery | jwks | token").req("answer", "see description").param("for", "10m").param("provider", "provider").route("POST", "/identity/{provider}/provoke-idp"),
            // ---------------------------------------------------------------- I6 OAuth clients
        Tool::new("list_oauth_clients", "Simulated clients (Stuntman as the client of the app's OAuth server) and their last run.", |ctx, _a| async move {
            let f = crate::clients::load(&ctx);
            let runs = crate::clients::runs(&ctx, None).await?;
            Ok(json!({"clients": f.clients.iter().map(|c| { let mut v = serde_json::to_value(c).unwrap_or(json!({})); if let Some(o) = v.as_object_mut() { o.remove("client_secret"); }
                v["metadataUrl"] = json!(crate::clients::metadata_url(&ctx, &c.id)); v["lastRun"] = runs.iter().find(|r| r["client"] == json!(c.id)).cloned().unwrap_or(Value::Null); v }).collect::<Vec<_>>(),
                "callback": crate::clients::callback(&ctx)}))
        }).route("GET", "/oauth-clients"),
        Tool::new("run_client_checks", "Runs the ten MCP authorization checks against an MCP server; pauses at the app's own sign-in page (open authorizeUrl, or sign in through the app's IdP) and continues when the code comes back.", |ctx, a| async move {
            let id = a.need("client")?;
            let mut f = crate::clients::load(&ctx);
            let c = f.clients.iter_mut().find(|c| c.id == id).ok_or_else(|| ApiError::not_found(format!("No client {id}")))?;
            if let Some(r) = a.str("resource_url") { c.resource = r; }
            if c.resource.is_empty() { return Err(ApiError::bad("resource_url: the MCP server, e.g. https://acme-api.stg.example.com/mcp")); }
            let c = c.clone();
            crate::clients::save(&ctx, &f)?;
            let run = crate::clients::start(&ctx, &c, &c.resource).await?;
            Ok(json!({"run": run, "message": "Running; get_run shows each check. It pauses at the sign-in: open authorizeUrl in a browser."}))
        }).req("client", "mcp-dcr | mcp-client | your client id").param("resource_url", "the MCP server URL").route("POST", "/oauth-clients/{client}/runs"),
        Tool::new("get_run", "One run: each check with what Stuntman sent and got; authorizeUrl while it waits for the sign-in.", |ctx, a| async move {
            let id = a.need("run_id")?;
            let (mut r, _, _) = store::get(&ctx, "run", &id).await?.ok_or_else(|| ApiError::not_found("No such run"))?;
            if let Some(o) = r.as_object_mut() { o.remove("verifier"); }
            r["id"] = json!(id);
            Ok(r)
        }).req("run_id", "run id").route("GET", "/oauth-clients/runs/{run_id}"),
        Tool::new("serve_client_metadata", "Creates or changes a simulated client: kind (dcr, metadata_url, preregistered), the MCP server it checks, the name in its metadata document; client_id/secret when pre-registered.", |ctx, a| async move {
            let id = a.need("client")?;
            config::valid_name(&id)?;
            let mut f = crate::clients::load(&ctx);
            let mut c = f.clients.iter().find(|c| c.id == id).cloned().unwrap_or(crate::clients::Client { id: id.clone(), name: id.clone(), kind: "dcr".into(), ..Default::default() });
            if let Some(v) = a.str("name") { c.name = v; }
            if let Some(v) = a.str("kind") { if !["dcr", "metadata_url", "preregistered"].contains(&v.as_str()) { return Err(ApiError::bad("kind: dcr, metadata_url or preregistered")); } c.kind = v; }
            if let Some(v) = a.str("resource_url") { c.resource = v; }
            if let Some(v) = a.str("client_name") { c.client_name = v; }
            if let Some(v) = a.str("client_id") { c.client_id = v; }
            if let Some(v) = a.str("client_secret") { c.client_secret = v; }
            match f.clients.iter_mut().find(|x| x.id == id) { Some(x) => *x = c.clone(), None => f.clients.push(c.clone()) }
            crate::clients::save(&ctx, &f)?;
            ctx.live.send("identity.runs", &id, "client");
            Ok(json!({"client": c.id, "metadataUrl": crate::clients::metadata_url(&ctx, &c.id), "metadata": crate::clients::metadata(&ctx, &c)}))
        }).req("client", "client id (letters, digits, dashes)").param("name", "label").param("kind", "dcr | metadata_url | preregistered").param("resource_url", "MCP server URL")
          .param("client_name", "name in the metadata document").param("client_id", "pre-registered id").param("client_secret", "pre-registered secret").route("PUT", "/oauth-clients/{client}"),
    ]
}
