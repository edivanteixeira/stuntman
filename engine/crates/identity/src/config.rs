//! An identity provider (identity/<name>.yaml, the source of truth): the tenant it answers for, the
//! flavour it imitates (Entra ID v2 first; Google, Okta and any OIDC as flavours), its synthetic users,
//! the app registrations, token lifetimes and the provider's own failures. Signing keys and the
//! sign-ins live in the database.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use stuntman_core::{util, yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Provider {
    pub kind: String,
    /// Short id in URLs and tools ("entra").
    pub name: String,
    pub environment: String,
    /// The tenant it answers for ("contoso-stg"); the issuer is …/login/<tenant>/v2.0.
    pub tenant: String,
    /// A stable GUID for the tid claim.
    pub tenant_id: String,
    /// entra-v2 | google | okta | oidc
    pub flavour: String,
    /// Tenants its users may come from, to test "wrong tenant".
    pub other_tenants: Vec<String>,
    pub keys: Keys,
    pub lifetimes: Lifetimes,
    pub provoke: Provoke,
    pub users: Vec<User>,
    pub apps: Vec<App>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Keys {
    pub current: String,
    pub next: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Lifetimes {
    pub id: String,
    pub access: String,
    pub refresh: String,
    pub clock_skew: String,
}

/// The provider's own failures (I5): discovery, JWKS and the token endpoint.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Provoke {
    pub discovery: String,
    pub jwks: String,
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct User {
    /// The person's handle in tools ("emma.carter").
    pub id: String,
    pub name: String,
    pub email: String,
    /// Stable object id: apps key users by it (oid / sub).
    pub oid: String,
    /// Home tenant; another one (from other_tenants) makes the user "from elsewhere".
    pub tenant: String,
    /// A guest from a personal account (idp claim, upn with #EXT#).
    pub guest: bool,
    pub groups: Vec<String>,
    pub roles: Vec<String>,
    pub mfa: bool,
    pub phone_lost: bool,
    pub enabled: bool,
    /// Apps the user consented to.
    pub consented: Vec<String>,
    /// Extra claims (employeeId, ctry…).
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct App {
    pub client_id: String,
    pub name: String,
    /// web (secret) | spa (PKCE, no secret) | daemon (client credentials) | api (exposes scopes)
    #[serde(rename = "type")]
    pub kind: String,
    pub redirect_uris: Vec<String>,
    /// secret://name (Key Vault or secrets.yaml) or a synthetic literal.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub secret: String,
    pub any_secret: bool,
    /// Scopes it asks for (openid profile email offline_access api://acme-api/orders.read).
    pub permissions: Vec<String>,
    /// Scopes an administrator consented to for everyone.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub admin_consent: Vec<String>,
    /// For type api: the scopes it exposes (orders.read) and its id URI (api://acme-api).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub exposes: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub app_id_uri: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub optional_claims: Vec<String>,
    /// Accounts in other tenants may sign in.
    pub multi_tenant: bool,
}

impl Default for Provider {
    fn default() -> Self {
        Self { kind: "identity-provider".into(), name: String::new(), environment: String::new(), tenant: String::new(), tenant_id: String::new(), flavour: "entra-v2".into(),
            other_tenants: vec![], keys: Keys::default(), lifetimes: Lifetimes::default(), provoke: Provoke::default(), users: vec![], apps: vec![] }
    }
}
impl Default for Lifetimes {
    fn default() -> Self {
        Self { id: "60m".into(), access: "60m".into(), refresh: "24h".into(), clock_skew: "0s".into() }
    }
}
impl Default for Provoke {
    fn default() -> Self {
        Self { discovery: "normal".into(), jwks: "normal".into(), token: "normal".into() }
    }
}

pub const FLAVOURS: [(&str, &str); 4] = [("entra-v2", "Microsoft Entra ID v2.0"), ("google", "Google"), ("okta", "Okta"), ("oidc", "Any OIDC provider")];

pub fn flavour_label(f: &str) -> &'static str {
    FLAVOURS.iter().find(|(k, _)| *k == f).map(|(_, l)| *l).unwrap_or("OIDC provider")
}

impl Lifetimes {
    pub fn secs(s: &str, default: i64) -> i64 {
        util::parse_ms(s).map(|m| m as i64 / 1000).unwrap_or(default)
    }
}

impl Provider {
    pub fn user(&self, who: &str) -> Option<&User> {
        let w = who.trim().to_lowercase();
        self.users.iter().find(|u| u.id.to_lowercase() == w || u.name.to_lowercase() == w || u.email.to_lowercase() == w || u.oid == w)
    }
    pub fn app(&self, id: &str) -> Option<&App> {
        self.apps.iter().find(|a| a.client_id == id || a.name == id || (!a.app_id_uri.is_empty() && a.app_id_uri == id))
    }
    /// The app that exposes a scope like api://acme-api/orders.read.
    pub fn api_for_scope(&self, scope: &str) -> Option<&App> {
        self.apps.iter().find(|a| !a.app_id_uri.is_empty() && scope.starts_with(&format!("{}/", a.app_id_uri)))
    }
}

pub fn dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("identity")
}

pub fn valid_name(n: &str) -> Result<(), ApiError> {
    let ok = !n.is_empty() && n.len() <= 32 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') && n.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
    if ok { Ok(()) } else { Err(ApiError::bad("Names: lowercase letters, digits and dashes, starting with a letter")) }
}

pub fn list(ctx: &Ctx) -> Vec<Provider> {
    let mut v: Vec<Provider> = std::fs::read_dir(dir(ctx)).map(|r| r.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .filter_map(|e| yamlfile::load::<Provider>(&e.path()).ok()).filter(|p| !p.name.is_empty()).collect()).unwrap_or_default();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Provider, ApiError> {
    list(ctx).into_iter().find(|p| p.name == name).ok_or_else(|| ApiError::not_found(format!("No identity provider {name} in {}", ctx.env)))
}

pub fn pick(ctx: &Ctx, name: Option<String>) -> Result<Provider, ApiError> {
    match name.filter(|n| !n.is_empty()) {
        Some(n) => get(ctx, &n),
        None => list(ctx).into_iter().next().ok_or_else(|| ApiError::not_found(format!("No identity provider in {} yet: create one (set_provider)", ctx.env))),
    }
}

/// The provider answering for a tenant in a URL (its own name, its tenant or tenant id; "common" and
/// "organizations" go to the first one).
pub fn by_tenant(ctx: &Ctx, tenant: &str) -> Option<Provider> {
    let all = list(ctx);
    all.iter().find(|p| p.tenant == tenant || p.tenant_id == tenant || p.name == tenant).cloned()
        .or_else(|| (["common", "organizations", "consumers"].contains(&tenant)).then(|| all.first().cloned()).flatten())
}

pub fn file_name(name: &str) -> String {
    format!("identity/{name}.yaml")
}

pub fn save(ctx: &Ctx, p: &Provider) -> Result<(), ApiError> {
    yamlfile::save(&dir(ctx).join(format!("{}.yaml", p.name)), p)
}

pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(dir(ctx).join(format!("{name}.yaml"))).unwrap_or_default()
}

/// A synthetic-looking email domain only: real public domains are refused for new users.
pub fn email_ok(e: &str) -> Result<(), ApiError> {
    let d = e.rsplit_once('@').map(|x| x.1.to_lowercase()).ok_or_else(|| ApiError::bad("Email: name@domain"))?;
    let synthetic = ["example.com", "example.org", "example.net", ".example.com", ".example", ".test", ".localhost", "ejemplo.com", ".invalid"];
    if synthetic.iter().any(|s| if s.starts_with('.') { d.ends_with(s) } else { d == *s || d.ends_with(&format!(".{s}")) }) { Ok(()) }
    else { Err(ApiError::bad("Synthetic people only: use a domain under example.com (e.g. contoso.example.com)")) }
}

/// A stable GUID from a seed (oid, tenant id), so files stay readable and ids never change.
pub fn guid(seed: &str) -> String {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(seed.as_bytes());
    let x: String = h.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
}

#[cfg(test)]
mod t {
    #[test]
    fn emails_and_guids() {
        assert!(super::email_ok("emma.carter@contoso.example.com").is_ok());
        assert!(super::email_ok("someone@gmail.com").is_err());
        assert_eq!(super::guid("a"), super::guid("a"));
        assert_eq!(super::guid("a").len(), 36);
    }
}
