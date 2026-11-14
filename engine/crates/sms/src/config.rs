//! The YAML files of an environment's SMS service (the source of truth): sms/service.yaml (which API
//! shape Stuntman answers, senders, callbacks, opt-out keywords, limits) and sms/recipients.yaml (the
//! simulated person behind each number).

use serde::{Deserialize, Serialize};
use stuntman_core::{util, yamlfile, ApiError, Ctx};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Service {
    pub kind: String,
    /// zenvia-v2 | twilio
    pub flavour: String,
    pub access: Access,
    pub senders: Vec<Sender>,
    /// An unknown "from" is answered 400 (as Zenvia does); off accepts any sender.
    pub strict_senders: bool,
    pub callbacks: Callbacks,
    pub opt_out: OptOut,
    pub limits: Limits,
    /// Zenvia SMS templates the app sends by templateId; their text fills {{field}} with the app's fields.
    /// An undeclared id is still sent (its fields are shown), unless strict_templates.
    pub templates: Vec<Template>,
    pub strict_templates: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub text: String,
}

/// "Your code is {{token}}" with {"token": "482913"}.
pub fn render(text: &str, fields: &serde_json::Value) -> String {
    let re = regex::Regex::new(r"\{\{\s*([\w.]+)\s*\}\}").unwrap();
    re.replace_all(text, |c: &regex::Captures| match fields.get(&c[1]) { Some(serde_json::Value::String(s)) => s.clone(), Some(v) if !v.is_null() => v.to_string(), _ => c[0].to_string() }).to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Access {
    pub any_token: bool,
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Sender {
    pub id: String,
    /// alphanumeric | short_code | long
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Callbacks {
    /// MESSAGE_STATUS; empty = the environment's default callback.
    pub status: String,
    /// MESSAGE (the person's replies); empty = the environment's default callback.
    pub inbound: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct OptOut {
    pub keywords: Vec<String>,
    pub back_in: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Limits {
    /// messages per second; over it: 429
    pub throughput: u32,
    /// over it: 400 "message too long"
    pub max_segments: u32,
}

impl Default for Service {
    fn default() -> Self {
        Self {
            kind: "sms".into(),
            flavour: "zenvia-v2".into(),
            access: Access::default(),
            senders: vec![Sender { id: "Acme".into(), kind: "alphanumeric".into() }, Sender { id: "28282".into(), kind: "short_code".into() }, Sender { id: "+15550104499".into(), kind: "long".into() }],
            strict_senders: false,
            callbacks: Callbacks::default(),
            opt_out: OptOut::default(),
            limits: Limits::default(),
            templates: vec![],
            strict_templates: false,
        }
    }
}
impl Default for Access {
    fn default() -> Self {
        Self { any_token: true, token: "secret://zenvia-token".into() }
    }
}
impl Default for OptOut {
    fn default() -> Self {
        Self { keywords: vec!["STOP".into(), "CANCEL".into(), "UNSUBSCRIBE".into(), "SAIR".into()], back_in: "START".into() }
    }
}
impl Default for Limits {
    fn default() -> Self {
        Self { throughput: 30, max_segments: 10 }
    }
}

/// When the text asks something, or on the nth text of the day: the person replies.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Reply {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,
    pub reply: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub after: String,
}

/// One step of a recipient; each sets one key (as in the YAML).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Step {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_after: Option<String>,
    /// The carrier never confirms: NOT_DELIVERED (expired) after this long (48h; test speed shortens it).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub never_delivered: Option<String>,
    /// NOT_DELIVERED with this reason: invalid | carrier_rejected | phone_off | blocked | not_covered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not_delivered: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_question: Option<Reply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_nth: Option<Reply>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_text: Option<Reply>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Recipient {
    /// "+14155550142", a prefix "+1312555 01*", or "*" (everyone else).
    pub number: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub carrier: String,
    pub steps: Vec<Step>,
}

impl Default for Recipient {
    fn default() -> Self {
        Self { number: "*".into(), carrier: String::new(), steps: vec![Step { delivered_after: Some("3s".into()), ..Default::default() }] }
    }
}

pub fn digits(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

pub fn reason_text(r: &str) -> &'static str {
    match r {
        "invalid" => "Invalid or inactive number",
        "carrier_rejected" => "Carrier rejected (filtered as spam)",
        "phone_off" => "Phone off: expired",
        "blocked" => "Number blocked by the person",
        "not_covered" => "Destination not covered",
        _ => "Not delivered",
    }
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
    pub fn delivered_ms(&self) -> u64 {
        self.steps.iter().find_map(|s| s.delivered_after.as_deref().and_then(util::parse_ms)).unwrap_or(3000)
    }
    pub fn not_delivered(&self) -> Option<String> {
        self.steps.iter().find_map(|s| s.not_delivered.clone())
    }
    pub fn never_delivered(&self) -> Option<u64> {
        self.steps.iter().find_map(|s| s.never_delivered.as_deref().map(|x| util::parse_ms(x).unwrap_or(172_800_000)))
    }
    pub fn carrier(&self) -> String {
        if self.carrier.is_empty() { "Simulated carrier".into() } else { format!("{} (simulated)", self.carrier) }
    }
}

fn file(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    ctx.path(&format!("sms/{name}"))
}

pub fn service(ctx: &Ctx) -> Result<Service, ApiError> {
    let p = file(ctx, "service.yaml");
    if !p.exists() {
        if util::resolve_secret("secret://zenvia-token", &ctx.data_dir).is_empty() {
            let _ = util::set_secret(&ctx.data_dir, "zenvia-token", &util::uuid().replace('-', ""));
        }
        save_service(ctx, &Service::default())?;
    }
    yamlfile::load(&p)
}
pub fn save_service(ctx: &Ctx, s: &Service) -> Result<(), ApiError> {
    yamlfile::save(&file(ctx, "service.yaml"), s)
}
pub fn recipients(ctx: &Ctx) -> Result<Vec<Recipient>, ApiError> {
    let p = file(ctx, "recipients.yaml");
    if !p.exists() {
        save_recipients(ctx, &[Recipient::default()])?;
    }
    yamlfile::load(&p)
}
pub fn save_recipients(ctx: &Ctx, r: &[Recipient]) -> Result<(), ApiError> {
    yamlfile::save(&file(ctx, "recipients.yaml"), &r)
}
pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(file(ctx, name)).unwrap_or_default()
}
pub fn recipient_for(ctx: &Ctx, number: &str) -> Recipient {
    recipients(ctx).ok().and_then(|r| r.into_iter().find(|x| x.matches(number))).unwrap_or_default()
}

/// GSM-7 or UCS-2, the character that forced UCS-2, and how many segments (160/153, 70/67).
pub fn encoding(text: &str) -> (String, Option<char>, usize, usize) {
    const GSM: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
    const EXT: &str = "^{}\\[~]|€\x0c";
    let bad = text.chars().find(|c| !GSM.contains(*c) && !EXT.contains(*c));
    match bad {
        None => {
            let units: usize = text.chars().map(|c| if EXT.contains(c) { 2 } else { 1 }).sum();
            let seg = if units <= 160 { 1 } else { units.div_ceil(153) };
            ("GSM-7".into(), None, units, seg)
        }
        Some(c) => {
            let units: usize = text.encode_utf16().count();
            let seg = if units <= 70 { 1 } else { units.div_ceil(67) };
            ("UCS-2".into(), Some(c), units, seg)
        }
    }
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn encodings_and_patterns() {
        assert_eq!(encoding("Acme: your code is 482913.").3, 1);
        let (e, c, n, s) = encoding("Acme: tu código es 739204.");
        assert_eq!((e.as_str(), c, s), ("UCS-2", Some('ó'), 1));
        assert_eq!(n, 26);
        assert_eq!(encoding(&"a".repeat(161)).3, 2);
        assert_eq!(encoding("é").0, "GSM-7");
        assert_eq!(render("Code {{token}} for {{ name }}", &serde_json::json!({"token": "482913", "name": "Acme"})), "Code 482913 for Acme");
        let r = Recipient { number: "+1 702 555 01*".into(), ..Default::default() };
        assert!(r.matches("+17025550199") && !r.matches("+17025550299"));
    }
}
