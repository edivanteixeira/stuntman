//! A14: an LDAP directory of a provider's people, for apps that still bind to Active Directory or
//! OpenLDAP. Simple bind (DN, user@domain, DOMAIN\user), search with filters, compare and whoami;
//! people under ou=people and groups under ou=groups of the provider's base DN. Wrong passwords,
//! locked and disabled accounts answer as Active Directory does (49, data 52e/775/533).
//! One port for every environment: binds and searches go to the default one.

use futures::{SinkExt, StreamExt};
use ldap3_proto::{
    proto::{LdapFilter, LdapPartialAttribute, LdapResultCode, LdapSearchResultEntry, LdapSearchScope},
    LdapCodec, ServerOps,
};
use stuntman_core::{env::Envs, Ctx};
use tokio_util::codec::Framed;

use crate::config::{self, Provider, User};

#[derive(Clone, Debug)]
pub struct Entry {
    pub dn: String,
    pub attrs: Vec<(String, Vec<String>)>,
}

impl Entry {
    fn get(&self, a: &str) -> Vec<&String> {
        self.attrs.iter().filter(|(k, _)| k.eq_ignore_ascii_case(a)).flat_map(|(_, v)| v.iter()).collect()
    }
}

pub fn user_dn(p: &Provider, u: &User) -> String {
    format!("uid={},ou=people,{}", u.id, p.base_dn())
}

fn group_dn(p: &Provider, g: &str) -> String {
    format!("cn={},ou=groups,{}", g, p.base_dn())
}

/// Every entry of a provider's directory.
pub fn entries(p: &Provider) -> Vec<Entry> {
    let base = p.base_dn();
    let s = |x: &str| vec![x.to_string()];
    let first = base.split(',').next().unwrap_or("").trim_start_matches("dc=").to_string();
    let mut out = vec![
        Entry { dn: base.clone(), attrs: vec![("objectClass".into(), vec!["top".into(), "domain".into()]), ("dc".into(), s(&first))] },
        Entry { dn: format!("ou=people,{base}"), attrs: vec![("objectClass".into(), vec!["top".into(), "organizationalUnit".into()]), ("ou".into(), s("people"))] },
        Entry { dn: format!("ou=groups,{base}"), attrs: vec![("objectClass".into(), vec!["top".into(), "organizationalUnit".into()]), ("ou".into(), s("groups"))] },
    ];
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for u in &p.users {
        let (given, sn) = u.name.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((u.name.clone(), u.name.clone()));
        let mut attrs = vec![
            ("objectClass".to_string(), vec!["top".into(), "person".into(), "organizationalPerson".into(), "inetOrgPerson".into(), "user".into()]),
            ("uid".into(), s(&u.id)), ("cn".into(), s(&u.name)), ("displayName".into(), s(&u.name)), ("givenName".into(), vec![given]), ("sn".into(), vec![sn]),
            ("mail".into(), s(&u.email)), ("userPrincipalName".into(), s(&u.email)), ("sAMAccountName".into(), s(&u.id)), ("entryUUID".into(), s(&u.oid)),
            ("userAccountControl".into(), s(if u.enabled { "512" } else { "514" })),
            ("memberOf".into(), u.groups.iter().map(|g| group_dn(p, g)).collect()),
        ];
        for (k, v) in &u.extra {
            attrs.push((k.clone(), vec![v.as_str().map(String::from).unwrap_or(v.to_string())]));
        }
        for g in &u.groups {
            groups.entry(g.clone()).or_default().push(user_dn(p, u));
        }
        out.push(Entry { dn: user_dn(p, u), attrs });
    }
    for (g, members) in groups {
        out.push(Entry { dn: group_dn(p, &g), attrs: vec![("objectClass".into(), vec!["top".into(), "groupOfNames".into(), "group".into()]), ("cn".into(), s(&g)), ("member".into(), members)] });
    }
    out
}

fn matches(e: &Entry, f: &LdapFilter) -> bool {
    let lc = |s: &str| s.to_lowercase();
    match f {
        LdapFilter::And(v) => v.iter().all(|x| matches(e, x)),
        LdapFilter::Or(v) => v.iter().any(|x| matches(e, x)),
        LdapFilter::Not(x) => !matches(e, x),
        LdapFilter::Present(a) => a.eq_ignore_ascii_case("objectClass") || !e.get(a).is_empty(),
        LdapFilter::Equality(a, v) | LdapFilter::Approx(a, v) => e.get(a).iter().any(|x| lc(x) == lc(v)),
        LdapFilter::Substring(a, s) => e.get(a).iter().any(|x| {
            let x = lc(x);
            let mut rest = x.as_str();
            if let Some(i) = &s.initial {
                if !rest.starts_with(&lc(i)) {
                    return false;
                }
                rest = &rest[i.len()..];
            }
            for any in &s.any {
                match rest.find(&lc(any)) {
                    Some(k) => rest = &rest[k + any.len()..],
                    None => return false,
                }
            }
            s.final_.as_ref().map(|f| rest.ends_with(&lc(f))).unwrap_or(true)
        }),
        LdapFilter::GreaterOrEqual(a, v) => e.get(a).iter().any(|x| lc(x) >= lc(v)),
        LdapFilter::LessOrEqual(a, v) => e.get(a).iter().any(|x| lc(x) <= lc(v)),
        LdapFilter::Extensible(_) => false,
    }
}

fn norm_dn(d: &str) -> String {
    d.split(',').map(|x| x.trim().to_lowercase()).collect::<Vec<_>>().join(",")
}

fn in_scope(entry_dn: &str, base: &str, scope: &LdapSearchScope) -> bool {
    let (e, b) = (norm_dn(entry_dn), norm_dn(base));
    match scope {
        LdapSearchScope::Base => e == b,
        LdapSearchScope::OneLevel => e.split_once(',').map(|(_, parent)| parent == b).unwrap_or(false),
        _ => b.is_empty() || e == b || e.ends_with(&format!(",{b}")),
    }
}

/// The provider a DN (or user@domain) belongs to.
fn provider_for(ctx: &Ctx, name: &str) -> Option<Provider> {
    let all: Vec<Provider> = config::list(ctx).into_iter().filter(|p| p.ldap.is_some()).collect();
    let n = norm_dn(name);
    all.iter().find(|p| !n.is_empty() && n.ends_with(&norm_dn(&p.base_dn()))).cloned().or_else(|| all.first().cloned())
}

/// The person a bind names: a full DN, user@domain, DOMAIN\user or the plain id.
pub fn bind_user<'a>(p: &'a Provider, dn: &str) -> Option<&'a User> {
    let d = dn.trim();
    if d.contains('=') {
        let first = d.split(',').next().unwrap_or("");
        let v = first.split_once('=').map(|x| x.1).unwrap_or("");
        return p.users.iter().find(|u| u.id.eq_ignore_ascii_case(v) || u.name.eq_ignore_ascii_case(v) || u.email.eq_ignore_ascii_case(v));
    }
    let id = d.rsplit('\\').next().unwrap_or(d);
    p.user(id).or_else(|| p.users.iter().find(|u| u.email.eq_ignore_ascii_case(d)))
}

const AD_BAD: &str = "80090308: LdapErr: DSID-0C09044E, comment: AcceptSecurityContext error, data 52e, v4563";

async fn conn(ctx: Ctx, sock: tokio::net::TcpStream) {
    let mut f = Framed::new(sock, LdapCodec::default());
    let mut bound = String::new();
    while let Some(Ok(msg)) = f.next().await {
        let Ok(op) = ServerOps::try_from(msg) else { break };
        let p_any = provider_for(&ctx, "");
        // The directory's own failures.
        if let Some(p) = &p_any {
            match p.ldap.as_ref().map(|l| l.provoke.as_str()).unwrap_or("") {
                "timeout" => {
                    tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                    break;
                }
                "busy" | "unavailable" => {
                    let code = if p.ldap.as_ref().unwrap().provoke == "busy" { LdapResultCode::Busy } else { LdapResultCode::Unavailable };
                    let r = match &op {
                        ServerOps::SimpleBind(b) => b.gen_error(code, "The server is busy (provoked in Stuntman)".into()),
                        ServerOps::Search(s) => s.gen_error(code, "The server is busy (provoked in Stuntman)".into()),
                        _ => break,
                    };
                    let _ = f.send(r).await;
                    continue;
                }
                _ => {}
            }
        }
        let reply = match op {
            ServerOps::SimpleBind(b) => {
                if b.dn.is_empty() && b.pw.is_empty() {
                    bound.clear();
                    b.gen_success()
                } else {
                    let p = provider_for(&ctx, &b.dn);
                    let u = p.as_ref().and_then(|p| bind_user(p, &b.dn)).cloned();
                    let (res, code, why) = match (&p, &u) {
                        (Some(_), Some(u)) if !u.enabled => ("refused", "data 533", format!("{} is disabled", u.name)),
                        (Some(_), Some(u)) if u.locked => ("refused", "data 775", format!("{} is locked out", u.name)),
                        (Some(p), Some(u)) if !p.password_ok(&ctx, u, &b.pw) => ("refused", "data 52e", format!("wrong password for {}", u.name)),
                        (Some(p), Some(u)) => {
                            bound = user_dn(p, u);
                            ("signed in", "", format!("{} bound to the directory", u.name))
                        }
                        _ => ("refused", "data 525", format!("no such user: {}", b.dn)),
                    };
                    if let Some(p) = &p {
                        let _ = crate::store::sign_in(&ctx, crate::store::SignIn { provider: p.name.clone(), user: u.as_ref().map(|x| x.id.clone()).unwrap_or_default(), app: "ldap".into(), kind: "ldap".into(),
                            result: res.into(), code: code.into(), sentence: format!("LDAP bind: {why}"), flow: "LDAP simple bind".into(), ..Default::default() }).await;
                    }
                    if res == "signed in" { b.gen_success() } else { b.gen_error(LdapResultCode::InvalidCredentials, AD_BAD.replace("data 52e", code)) }
                }
            }
            ServerOps::Search(s) => {
                if s.base.is_empty() && matches!(s.scope, LdapSearchScope::Base) {
                    let ctxs: Vec<Vec<u8>> = config::list(&ctx).iter().filter(|p| p.ldap.is_some()).map(|p| p.base_dn().into_bytes()).collect();
                    let root = LdapSearchResultEntry { dn: String::new(), attributes: vec![
                        LdapPartialAttribute { atype: "namingContexts".into(), vals: ctxs.clone() },
                        LdapPartialAttribute { atype: "defaultNamingContext".into(), vals: ctxs.into_iter().take(1).collect() },
                        LdapPartialAttribute { atype: "supportedLDAPVersion".into(), vals: vec![b"3".to_vec()] },
                        LdapPartialAttribute { atype: "vendorName".into(), vals: vec![b"Stuntman".to_vec()] }] };
                    let _ = f.send(s.gen_result_entry(root)).await;
                    s.gen_success()
                } else {
                    let p = provider_for(&ctx, &s.base);
                    let all = p.as_ref().map(entries).unwrap_or_default();
                    let want: Vec<String> = s.attrs.iter().map(|a| a.to_lowercase()).collect();
                    let every = want.is_empty() || want.iter().any(|a| a == "*");
                    for e in all.iter().filter(|e| in_scope(&e.dn, &s.base, &s.scope) && matches(e, &s.filter)) {
                        let attributes = e.attrs.iter().filter(|(k, v)| !v.is_empty() && (every || want.contains(&k.to_lowercase()))).map(|(k, v)| LdapPartialAttribute { atype: k.clone(), vals: v.iter().map(|x| x.clone().into_bytes()).collect() }).collect();
                        let _ = f.send(s.gen_result_entry(LdapSearchResultEntry { dn: e.dn.clone(), attributes })).await;
                    }
                    s.gen_success()
                }
            }
            ServerOps::Compare(c) => {
                let p = provider_for(&ctx, &c.entry);
                let e = p.as_ref().map(entries).unwrap_or_default().into_iter().find(|e| norm_dn(&e.dn) == norm_dn(&c.entry));
                match e {
                    Some(e) if e.get(&c.atype).iter().any(|v| v.eq_ignore_ascii_case(&c.val)) => c.gen_compare_true(),
                    Some(_) => c.gen_compare_false(),
                    None => c.gen_error(LdapResultCode::NoSuchObject, format!("No such entry: {}", c.entry)),
                }
            }
            ServerOps::Whoami(w) => w.gen_success(&if bound.is_empty() { String::new() } else { format!("dn: {bound}") }),
            ServerOps::Unbind(_) => break,
            ServerOps::Abandon { .. } => continue,
        };
        if f.send(reply).await.is_err() {
            break;
        }
    }
}

pub fn serve(envs: Envs, port: u16) {
    tokio::spawn(async move {
        let Ok(l) = tokio::net::TcpListener::bind(("0.0.0.0", port)).await else {
            eprintln!("LDAP: port {port} is taken");
            return;
        };
        loop {
            let Ok((sock, _)) = l.accept().await else { continue };
            if let Some(ctx) = envs.default_env() {
                tokio::spawn(conn(ctx, sock));
            }
        }
    });
}
