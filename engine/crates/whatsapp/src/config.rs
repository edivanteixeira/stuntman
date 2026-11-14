//! The YAML files of an environment's WhatsApp number (the source of truth): whatsapp/number.yaml,
//! whatsapp/templates.yaml and whatsapp/recipients.yaml. The screens and tools edit these files.

use stuntman_core::{util, yamlfile, ApiError, Ctx};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Number {
    pub kind: String,
    pub phone_number_id: String,
    pub display: Display,
    pub waba_id: String,
    pub api_versions: String,
    pub access: Access,
    pub webhook: Hook,
    pub limits: Limits,
    /// green | yellow | red | flagged | restricted
    pub quality: String,
    /// Templates the app creates through the API stay PENDING this long, then approve.
    pub approve_after: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_expired_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Display {
    pub number: String,
    pub name: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Access {
    pub any_token: bool,
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Hook {
    /// Where Stuntman sends Meta's webhooks; empty = the environment's default callback.
    pub url: String,
    pub verify_token: String,
    pub app_secret: String,
    /// X-Hub-Signature-256 on every webhook.
    pub sign: bool,
    pub fields: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Limits {
    /// 250 | 1k | 10k | 100k | unlimited: different numbers per 24 h.
    pub tier: String,
    /// messages per second
    pub throughput: u32,
}

impl Default for Number {
    fn default() -> Self {
        Self {
            kind: "meta-whatsapp".into(),
            phone_number_id: "105954558954427".into(),
            display: Display { number: "+15550104477".into(), name: "Acme Store".into() },
            waba_id: "102290129340398".into(),
            api_versions: "any".into(),
            access: Access::default(),
            webhook: Hook::default(),
            limits: Limits::default(),
            quality: "green".into(),
            approve_after: "5m".into(),
            tokens_expired_at: None,
        }
    }
}
impl Default for Access {
    fn default() -> Self {
        Self { any_token: true, token: "secret://whatsapp-token".into() }
    }
}
impl Default for Hook {
    fn default() -> Self {
        Self {
            url: String::new(),
            verify_token: "secret://whatsapp-verify-token".into(),
            app_secret: "secret://whatsapp-app-secret".into(),
            sign: true,
            fields: vec!["messages".into(), "message_template_status_update".into(), "template_category_update".into()],
        }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self { tier: "1k".into(), throughput: 80 }
    }
}

impl Limits {
    pub fn tier_max(&self) -> Option<usize> {
        match self.tier.to_lowercase().as_str() {
            "250" => Some(250),
            "1k" | "1000" => Some(1000),
            "10k" => Some(10_000),
            "100k" => Some(100_000),
            _ => None,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Button {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// The payload the app gets when the person taps it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub quick_reply: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Template {
    pub name: String,
    /// utility | marketing | authentication
    pub category: String,
    pub languages: Vec<String>,
    /// approved | pending | rejected | paused | disabled
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    /// high | medium | low
    pub quality: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub header: String,
    pub body: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub footer: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Button>,
    /// What each {{n}} means, for the screen.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<String>,
    /// Set when the app created it through the API (auto approval counts from here).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<i64>,
}

impl Default for Template {
    fn default() -> Self {
        Self { name: String::new(), category: "utility".into(), languages: vec!["en_US".into()], status: "approved".into(), reason: String::new(), quality: "high".into(),
               header: String::new(), body: String::new(), footer: String::new(), buttons: vec![], variables: vec![], created_at: None }
    }
}

impl Template {
    /// Number of {{n}} placeholders in the body.
    pub fn body_params(&self) -> usize {
        params_in(&self.body)
    }
    pub fn id(&self) -> String {
        // A stable numeric id, as Meta gives one.
        let h = self.name.bytes().fold(1469598103934665603u64, |a, b| (a ^ b as u64).wrapping_mul(1099511628211));
        (h % 9_000_000_000_000_000 + 1_000_000_000_000_000).to_string()
    }
}

pub fn params_in(s: &str) -> usize {
    let re = regex::Regex::new(r"\{\{\s*(\d+)\s*\}\}").unwrap();
    re.captures_iter(s).filter_map(|c| c[1].parse::<usize>().ok()).max().unwrap_or(0)
}

/// Fills {{1}}… with the values the app sent.
pub fn render(s: &str, values: &[String]) -> String {
    let re = regex::Regex::new(r"\{\{\s*(\d+)\s*\}\}").unwrap();
    re.replace_all(s, |c: &regex::Captures| c[1].parse::<usize>().ok().and_then(|i| values.get(i.wrapping_sub(1)).cloned()).unwrap_or_else(|| c[0].to_string())).to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct After {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tap: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reply: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub caption: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub after: String,
}

/// One step of a recipient; each sets one key (as in the YAML).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Step {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_after: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub never_read: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stay_at_sent: Option<bool>,
    /// A Meta error code: the message fails with it after "sent".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_buttons: Option<After>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_text: Option<After>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_image: Option<After>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_location: Option<After>,
    /// The person blocks the business after this many messages.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_after: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Recipient {
    /// "+14155550142", a prefix pattern "+1312555 01*", or "*" (everyone else).
    pub number: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub profile_name: String,
    pub whatsapp: bool,
    pub steps: Vec<Step>,
}

impl Default for Recipient {
    fn default() -> Self {
        Self { number: "*".into(), profile_name: String::new(), whatsapp: true, steps: vec![] }
    }
}

pub fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

impl Recipient {
    pub fn matches(&self, number: &str) -> bool {
        let n = digits(number);
        let p = self.number.trim();
        if p == "*" {
            return true;
        }
        match p.strip_suffix('*') {
            Some(pre) => n.starts_with(&digits(pre)),
            None => n == digits(p),
        }
    }
    pub fn delivered_ms(&self) -> Option<u64> {
        if self.steps.iter().any(|s| s.stay_at_sent == Some(true)) {
            return None;
        }
        Some(self.steps.iter().find_map(|s| s.delivered_after.as_deref().and_then(util::parse_ms)).unwrap_or(1000))
    }
    pub fn read_ms(&self) -> Option<u64> {
        if self.steps.iter().any(|s| s.never_read == Some(true) || s.stay_at_sent == Some(true)) {
            return None;
        }
        Some(self.steps.iter().find_map(|s| s.read_after.as_deref().and_then(util::parse_ms)).unwrap_or(30_000))
    }
    pub fn fail_code(&self) -> Option<u32> {
        self.steps.iter().find_map(|s| s.fail)
    }
    pub fn block_after(&self) -> Option<u32> {
        self.steps.iter().find_map(|s| s.block_after)
    }
}

fn file(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    ctx.path(&format!("whatsapp/{name}"))
}

pub fn number(ctx: &Ctx) -> Result<Number, ApiError> {
    let p = file(ctx, "number.yaml");
    if !p.exists() {
        // The number's secrets are made once, in the environment's secrets (never shown on screen).
        for name in ["whatsapp-token", "whatsapp-verify-token", "whatsapp-app-secret"] {
            if util::resolve_secret(&format!("secret://{name}"), &ctx.data_dir).is_empty() {
                let _ = util::set_secret(&ctx.data_dir, name, &util::uuid().replace('-', ""));
            }
        }
        save_number(ctx, &Number::default())?;
    }
    yamlfile::load(&p)
}
pub fn save_number(ctx: &Ctx, n: &Number) -> Result<(), ApiError> {
    yamlfile::save(&file(ctx, "number.yaml"), n)
}
pub fn templates(ctx: &Ctx) -> Result<Vec<Template>, ApiError> {
    yamlfile::load(&file(ctx, "templates.yaml"))
}
pub fn save_templates(ctx: &Ctx, t: &[Template]) -> Result<(), ApiError> {
    yamlfile::save(&file(ctx, "templates.yaml"), &t)
}
pub fn recipients(ctx: &Ctx) -> Result<Vec<Recipient>, ApiError> {
    let p = file(ctx, "recipients.yaml");
    if !p.exists() {
        let d = vec![Recipient { steps: vec![Step { delivered_after: Some("1s".into()), ..Default::default() }, Step { read_after: Some("30s".into()), ..Default::default() }], ..Default::default() }];
        save_recipients(ctx, &d)?;
    }
    yamlfile::load(&p)
}
pub fn save_recipients(ctx: &Ctx, r: &[Recipient]) -> Result<(), ApiError> {
    yamlfile::save(&file(ctx, "recipients.yaml"), &r)
}
pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(file(ctx, name)).unwrap_or_default()
}

/// The recipient rule for a number (first match; "*" catches the rest).
pub fn recipient_for(ctx: &Ctx, number: &str) -> Recipient {
    recipients(ctx).ok().and_then(|r| r.into_iter().find(|x| x.matches(number))).unwrap_or_else(|| Recipient {
        steps: vec![Step { delivered_after: Some("1s".into()), ..Default::default() }, Step { read_after: Some("30s".into()), ..Default::default() }],
        ..Default::default()
    })
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn patterns_and_render() {
        let r = Recipient { number: "+1 312 555 01*".into(), ..Default::default() };
        assert!(r.matches("+13125550110") && !r.matches("+13125550210"));
        assert!(Recipient::default().matches("+44 20 7946 0958"));
        assert_eq!(params_in("Hi {{1}}, order {{2}} total {{3}} on {{4}}"), 4);
        assert_eq!(render("Hi {{1}}, order {{2}}", &["Emma".into(), "58213".into()]), "Hi Emma, order 58213");
        let r = Recipient { steps: vec![Step { never_read: Some(true), ..Default::default() }], ..Default::default() };
        assert_eq!((r.delivered_ms(), r.read_ms()), (Some(1000), None));
    }
}
