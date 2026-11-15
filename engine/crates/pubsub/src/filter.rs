//! The subscription filter language: `attributes.k = "v"`, `attributes.k != "v"`, `attributes:k`,
//! `hasPrefix(attributes.k, "x")`, `NOT` (or `-`), `AND`, `OR` and parentheses. A message the
//! filter does not match is never delivered (Pub/Sub acks it for the subscriber).

use crate::store::Labels;

#[derive(Debug, PartialEq)]
pub enum F {
    Has(String),
    Eq(String, String),
    Ne(String, String),
    Prefix(String, String),
    Not(Box<F>),
    And(Box<F>, Box<F>),
    Or(Box<F>, Box<F>),
}

impl F {
    pub fn eval(&self, a: &Labels) -> bool {
        match self {
            F::Has(k) => a.contains_key(k),
            F::Eq(k, v) => a.get(k) == Some(v),
            F::Ne(k, v) => a.get(k) != Some(v),
            F::Prefix(k, p) => a.get(k).is_some_and(|x| x.starts_with(p.as_str())),
            F::Not(f) => !f.eval(a),
            F::And(x, y) => x.eval(a) && y.eval(a),
            F::Or(x, y) => x.eval(a) || y.eval(a),
        }
    }
}

/// Whether a filter (empty = everything) lets a message with these attributes through.
pub fn matches(filter: &str, a: &Labels) -> bool {
    filter.trim().is_empty() || parse(filter).map(|f| f.eval(a)).unwrap_or(true)
}

#[derive(Debug, Clone, PartialEq)]
enum T {
    Word(String),
    Str(String),
    Sym(&'static str),
}

fn lex(s: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = s.chars().collect();
    let (mut i, mut out) = (0, vec![]);
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() { i += 1; continue }
        if ch == '"' || ch == '\'' {
            let mut v = String::new();
            i += 1;
            while i < c.len() && c[i] != ch {
                if c[i] == '\\' && i + 1 < c.len() { i += 1 }
                v.push(c[i]);
                i += 1;
            }
            if i >= c.len() { return Err("unterminated string".into()) }
            i += 1;
            out.push(T::Str(v));
            continue;
        }
        if ch == '!' && c.get(i + 1) == Some(&'=') { out.push(T::Sym("!=")); i += 2; continue }
        if let Some(sym) = ["(", ")", "=", ":", ",", "-"].into_iter().find(|x| x.starts_with(ch)) { out.push(T::Sym(sym)); i += 1; continue }
        if ch.is_alphanumeric() || ch == '_' || ch == '.' {
            let mut j = i;
            while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_' || c[j] == '.' || c[j] == '-') { j += 1 }
            out.push(T::Word(c[i..j].iter().collect()));
            i = j;
            continue;
        }
        return Err(format!("unexpected character '{ch}'"));
    }
    Ok(out)
}

struct P {
    t: Vec<T>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Result<T, String> {
        let t = self.t.get(self.i).cloned().ok_or("unexpected end of the filter")?;
        self.i += 1;
        Ok(t)
    }
    fn word(&self, w: &str) -> bool {
        self.peek() == Some(&T::Word(w.into()))
    }
    fn sym(&mut self, s: &'static str) -> Result<(), String> {
        match self.next()? { T::Sym(x) if x == s => Ok(()), t => Err(format!("expected '{s}', found {t:?}")) }
    }
    fn or(&mut self) -> Result<F, String> {
        let mut f = self.and()?;
        while self.word("OR") { self.i += 1; f = F::Or(Box::new(f), Box::new(self.and()?)); }
        Ok(f)
    }
    fn and(&mut self) -> Result<F, String> {
        let mut f = self.not()?;
        while self.word("AND") { self.i += 1; f = F::And(Box::new(f), Box::new(self.not()?)); }
        Ok(f)
    }
    fn not(&mut self) -> Result<F, String> {
        if self.word("NOT") || self.peek() == Some(&T::Sym("-")) {
            self.i += 1;
            return Ok(F::Not(Box::new(self.not()?)));
        }
        if self.peek() == Some(&T::Sym("(")) {
            self.i += 1;
            let f = self.or()?;
            self.sym(")")?;
            return Ok(f);
        }
        self.atom()
    }
    fn key(&mut self) -> Result<String, String> {
        match self.next()? {
            T::Word(w) => w.strip_prefix("attributes.").filter(|k| !k.is_empty()).map(String::from).ok_or_else(|| format!("expected attributes.<key>, found {w}")),
            t => Err(format!("expected attributes.<key>, found {t:?}")),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        match self.next()? { T::Str(s) => Ok(s), t => Err(format!("expected a quoted string, found {t:?}")) }
    }
    fn atom(&mut self) -> Result<F, String> {
        if self.word("hasPrefix") {
            self.i += 1;
            self.sym("(")?;
            let k = self.key()?;
            self.sym(",")?;
            let p = self.string()?;
            self.sym(")")?;
            return Ok(F::Prefix(k, p));
        }
        if self.word("attributes") {
            self.i += 1;
            self.sym(":")?;
            return match self.next()? { T::Word(k) | T::Str(k) => Ok(F::Has(k)), t => Err(format!("expected an attribute name, found {t:?}")) };
        }
        let k = self.key()?;
        match self.next()? {
            T::Sym("=") => Ok(F::Eq(k, self.string()?)),
            T::Sym("!=") => Ok(F::Ne(k, self.string()?)),
            t => Err(format!("expected = or != after attributes.{k}, found {t:?}")),
        }
    }
}

pub fn parse(s: &str) -> Result<F, String> {
    if s.len() > 256 { return Err("A filter is at most 256 bytes".into()) }
    let mut p = P { t: lex(s)?, i: 0 };
    let f = p.or()?;
    if p.i < p.t.len() { return Err(format!("unexpected {:?}", p.t[p.i])) }
    Ok(f)
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn filters() {
        let a: Labels = [("region", "us-east"), ("kind", "delivered"), ("carrier", "globex-freight")].into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        for (f, want) in [
            (r#"attributes.region = "us-east""#, true),
            (r#"attributes.region != "us-east""#, false),
            ("attributes:kind", true),
            ("NOT attributes:priority", true),
            ("-attributes:kind", false),
            (r#"hasPrefix(attributes.carrier, "globex")"#, true),
            (r#"attributes.kind = "created" OR (attributes:carrier AND hasPrefix(attributes.region, "us-"))"#, true),
            (r#"attributes.kind = "delivered" AND attributes.region = "eu-west""#, false),
        ] {
            assert_eq!(parse(f).unwrap().eval(&a), want, "{f}");
        }
        for bad in ["attributes.region = us-east", "region = \"x\"", "attributes.k =", "hasPrefix(attributes.k)", "(attributes:k"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }
}
