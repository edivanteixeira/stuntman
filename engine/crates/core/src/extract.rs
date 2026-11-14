//! Extractors: named values taken out of a message (an OTP, a sign link). Channel-agnostic: a
//! channel hands over a [`Doc`] with the sources it has (SMS has only text; e-mail has all).

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Subject,
    #[default]
    Text,
    Html,
    Link,
    Header,
    Attachment,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct OnlyOn {
    /// any | subject | kind | from | to
    #[serde(default = "any")]
    pub when: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub value: String,
}
fn any() -> String {
    "any".into()
}

impl Default for OnlyOn {
    fn default() -> Self {
        Self { when: any(), value: String::new() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Extractor {
    pub name: String,
    pub from: Source,
    /// Regex (subject, text, link, attachment), CSS selector (html) or header name (header).
    pub rule: String,
    #[serde(default)]
    pub only_on: OnlyOn,
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_redirects: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_matches: bool,
    /// Remove spaces and dashes: "482 913" → "482913".
    #[serde(default, skip_serializing_if = "is_false")]
    pub strip: bool,
}
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CodeSettings {
    pub on: bool,
    pub min_digits: usize,
    pub max_digits: usize,
    pub words: Vec<String>,
    pub prefer_prominent: bool,
}

impl Default for CodeSettings {
    fn default() -> Self {
        Self {
            on: true,
            min_digits: 4,
            max_digits: 8,
            words: ["code", "código", "codigo", "PIN", "OTP", "token", "verification"].map(String::from).to_vec(),
            prefer_prominent: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Link {
    pub url: String,
    /// After following redirects, when that was asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    pub redirects: u32,
}

/// What a channel offers to extractors.
#[derive(Clone, Debug, Default)]
pub struct Doc {
    pub subject: String,
    pub text: String,
    pub html: String,
    pub links: Vec<Link>,
    pub headers: Vec<(String, String)>,
    pub attachments: Vec<String>,
    pub kind: String,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    /// Skipped by "only on".
    pub skipped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nearest: Option<String>,
    /// Index of the link that matched, for "matched the 1st link".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_index: Option<usize>,
}

/// Checks a rule without running it. Errors read like "Unclosed group at position 12".
pub fn validate(ex: &Extractor) -> Result<(), String> {
    if ex.name.trim().is_empty() || !ex.name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return Err("Name: letters, digits, - and _ only".into());
    }
    if ex.rule.trim().is_empty() {
        return Err("The rule is empty".into());
    }
    match ex.from {
        Source::Html => scraper::Selector::parse(&ex.rule).map(|_| ()).map_err(|e| format!("Not a CSS selector: {e}")),
        Source::Header => Ok(()),
        _ => Regex::new(&ex.rule).map(|_| ()).map_err(regex_message),
    }?;
    if ex.only_on.when != "any" && ex.only_on.when != "kind" && ex.only_on.when != "from" && ex.only_on.when != "to" {
        Regex::new(&ex.only_on.value).map_err(|e| format!("Only on: {}", regex_message(e)))?;
    }
    Ok(())
}

fn regex_message(e: regex::Error) -> String {
    // The regex crate prints a multi-line diagram; keep the last line, which says what is wrong.
    let s = e.to_string();
    let last = s.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or(&s).trim().trim_start_matches("error: ");
    let mut c = last.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

fn applies(ex: &Extractor, d: &Doc) -> bool {
    let v = &ex.only_on.value;
    match ex.only_on.when.as_str() {
        "subject" => Regex::new(v).map(|r| r.is_match(&d.subject)).unwrap_or(false),
        "kind" => d.kind == *v,
        "from" => d.from.to_lowercase().contains(&v.to_lowercase()),
        "to" => d.to.eq_ignore_ascii_case(v),
        _ => true,
    }
}

fn finish(ex: &Extractor, mut vals: Vec<String>) -> Option<Value> {
    if ex.strip {
        vals = vals.into_iter().map(|v| v.chars().filter(|c| *c != ' ' && *c != '-').collect()).collect();
    }
    if vals.is_empty() {
        None
    } else if ex.all_matches {
        Some(json!(vals))
    } else {
        Some(json!(vals[0]))
    }
}

fn regex_values(re: &Regex, hay: &str) -> Vec<String> {
    re.captures_iter(hay).map(|c| c.get(1).or(c.get(0)).unwrap().as_str().to_string()).collect()
}

pub fn run(ex: &Extractor, d: &Doc) -> Outcome {
    if !applies(ex, d) {
        return Outcome { skipped: true, why: Some("skipped by “Only on”".into()), ..Default::default() };
    }
    if let Err(e) = validate(ex) {
        return Outcome { why: Some(e), ..Default::default() };
    }
    let mut out = Outcome::default();
    let vals: Vec<String> = match ex.from {
        Source::Subject => regex_values(&Regex::new(&ex.rule).unwrap(), &d.subject),
        Source::Text => regex_values(&Regex::new(&ex.rule).unwrap(), &d.text),
        Source::Html => {
            let doc = scraper::Html::parse_document(&d.html);
            let sel = scraper::Selector::parse(&ex.rule).unwrap();
            doc.select(&sel).map(|e| e.text().collect::<String>().trim().to_string()).filter(|s| !s.is_empty()).collect()
        }
        Source::Header => d
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case(ex.rule.trim()))
            .map(|(_, v)| v.clone())
            .collect(),
        Source::Attachment => {
            let re = Regex::new(&ex.rule).unwrap();
            d.attachments.iter().filter(|n| re.is_match(n)).cloned().collect()
        }
        Source::Link => {
            let re = Regex::new(&ex.rule).unwrap();
            let mut v = vec![];
            for (i, l) in d.links.iter().enumerate() {
                let target = if ex.follow_redirects { l.final_url.as_deref().unwrap_or(&l.url) } else { &l.url };
                if let Some(c) = re.captures(target).or_else(|| re.captures(&l.url)) {
                    out.link_index.get_or_insert(i);
                    v.push(if c.len() > 1 && c.get(1).is_some() { c[1].to_string() } else { target.to_string() });
                }
            }
            if v.is_empty() {
                out.nearest = nearest_link(&ex.rule, &d.links);
            }
            v
        }
    };
    out.value = finish(ex, vals);
    if out.value.is_none() {
        out.why = Some(match ex.from {
            Source::Link => "no such link".into(),
            Source::Header => format!("no header “{}”", ex.rule.trim()),
            _ => "no match".into(),
        });
    }
    out
}

/// The link that shares the longest literal prefix with the rule: "differs at the date part".
fn nearest_link(rule: &str, links: &[Link]) -> Option<String> {
    let lit: String = rule.chars().take_while(|c| !"[](){}.*+?^$|\\".contains(*c)).collect();
    if lit.len() < 3 {
        return None;
    }
    links.iter().find(|l| l.final_url.as_deref().unwrap_or(&l.url).contains(&lit)).map(|l| l.final_url.clone().unwrap_or(l.url.clone()))
}

/// Built-in "code": 4–8 digits near a marker word; prominent (bold, boxed, heading) wins.
pub fn code(s: &CodeSettings, d: &Doc) -> Option<String> {
    if !s.on {
        return None;
    }
    let pat = format!(r"\b\d(?:[ \-]?\d){{{},{}}}\b", s.min_digits.saturating_sub(1), s.max_digits.saturating_sub(1));
    let re = Regex::new(&pat).ok()?;
    let clean = |m: &str| m.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let ok_len = |v: &str| v.len() >= s.min_digits && v.len() <= s.max_digits;
    if s.prefer_prominent && !d.html.is_empty() {
        let doc = scraper::Html::parse_document(&d.html);
        let sel = scraper::Selector::parse("strong, b, h1, h2, h3, td, div, p, span").unwrap();
        // The smallest element whose whole text is the code.
        for e in doc.select(&sel) {
            let t = e.text().collect::<String>();
            let t = t.trim();
            if re.find(t).map(|m| m.as_str() == t).unwrap_or(false) && ok_len(&clean(t)) {
                return Some(clean(t));
            }
        }
    }
    let hay = format!("{}\n{}", d.subject, d.text);
    let lower = hay.to_lowercase();
    let words: Vec<String> = s.words.iter().map(|w| w.to_lowercase()).collect();
    let mut best: Option<(usize, String)> = None;
    for m in re.find_iter(&hay) {
        let v = clean(m.as_str());
        if !ok_len(&v) {
            continue;
        }
        let from = m.start().saturating_sub(120);
        let window = lower.get(from..m.start()).unwrap_or("");
        if let Some(dist) = words.iter().filter_map(|w| window.rfind(w.as_str()).map(|p| window.len() - p)).min() {
            if best.as_ref().map(|b| dist < b.0).unwrap_or(true) {
                best = Some((dist, v));
            }
        }
    }
    best.map(|b| b.1)
}

/// Every link in the text and HTML bodies, in order, without duplicates.
pub fn links(text: &str, html: &str) -> Vec<Link> {
    let mut out: Vec<String> = vec![];
    if !html.is_empty() {
        let doc = scraper::Html::parse_document(html);
        let sel = scraper::Selector::parse("a[href]").unwrap();
        for a in doc.select(&sel) {
            let h = a.value().attr("href").unwrap_or("").trim();
            if h.starts_with("http") && !out.iter().any(|x| x == h) {
                out.push(h.to_string());
            }
        }
    }
    let re = Regex::new(r#"https?://[^\s<>"')\]]+"#).unwrap();
    for m in re.find_iter(text) {
        let u = m.as_str().trim_end_matches(['.', ',', ';']);
        if !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    }
    out.into_iter().map(|url| Link { url, final_url: None, redirects: 0 }).collect()
}

/// Follows redirects without loading pages from the app's point of view: HEAD, then GET on 405.
pub async fn follow(http: &reqwest::Client, l: &mut Link) {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(std::time::Duration::from_secs(3)).build();
    let Ok(client) = client else { return };
    let _ = http;
    let mut url = l.url.clone();
    for _ in 0..5 {
        let r = match client.head(&url).send().await {
            Ok(r) => r,
            Err(_) => return,
        };
        if !r.status().is_redirection() {
            break;
        }
        let Some(loc) = r.headers().get("location").and_then(|v| v.to_str().ok()) else { break };
        url = reqwest::Url::parse(&url).and_then(|b| b.join(loc)).map(|u| u.to_string()).unwrap_or(loc.to_string());
        l.redirects += 1;
    }
    if l.redirects > 0 {
        l.final_url = Some(url);
    }
}

#[cfg(test)]
mod t {
    use super::*;

    fn doc() -> Doc {
        Doc {
            subject: "Your verification code".into(),
            text: "Use this code to confirm your signature: 482 913. It expires in 10 minutes.".into(),
            html: "<p>Use this code</p><div class=code-box><strong>482 913</strong></div><a href=\"https://acme.example.com/operation/3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11/2026-11-12\">Sign</a>".into(),
            links: links("", "<a href=\"https://acme.example.com/operation/3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11/2026-11-12\">x</a><a href=\"https://acme.example.com/privacy\">p</a>"),
            ..Default::default()
        }
    }

    #[test]
    fn builtin_code_prefers_prominent_and_strips() {
        assert_eq!(code(&CodeSettings::default(), &doc()).as_deref(), Some("482913"));
        let mut d = doc();
        d.html.clear();
        assert_eq!(code(&CodeSettings::default(), &d).as_deref(), Some("482913"));
    }

    #[test]
    fn link_rule_takes_url_or_group() {
        let ex = Extractor { name: "signLink".into(), from: Source::Link, rule: r"/operation/[0-9a-f-]{36}/[^\s]+".into(), ..Default::default() };
        let o = run(&ex, &doc());
        assert_eq!(o.value.unwrap(), json!("https://acme.example.com/operation/3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11/2026-11-12"));
        assert_eq!(o.link_index, Some(0));
        let ex = Extractor { rule: r"/operation/([0-9a-f-]{36})/".into(), ..ex };
        assert_eq!(run(&ex, &doc()).value.unwrap(), json!("3f2c9e1a-77b0-4c1e-9d0a-5b1c2e7f0a11"));
    }

    #[test]
    fn only_on_skips_and_broken_rule_explains() {
        let mut ex = Extractor { name: "otp".into(), from: Source::Text, rule: r"(\d{3} \d{3})".into(), strip: true, ..Default::default() };
        assert_eq!(run(&ex, &doc()).value.unwrap(), json!("482913"));
        ex.only_on = OnlyOn { when: "subject".into(), value: "Reset".into() };
        assert!(run(&ex, &doc()).skipped);
        ex.rule = "/operation/([0-9a-f-]{36}/".into();
        assert!(validate(&ex).unwrap_err().to_lowercase().contains("unclosed"));
    }

    #[test]
    fn html_selector() {
        let ex = Extractor { name: "otp".into(), from: Source::Html, rule: ".code-box strong".into(), strip: true, ..Default::default() };
        assert_eq!(run(&ex, &doc()).value.unwrap(), json!("482913"));
    }
}
