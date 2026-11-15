//! JMS message selectors (the SQL-92 subset of the JMS spec, as ActiveMQ Classic reads them):
//! = <> < > <= >=, AND OR NOT, [NOT] IN, [NOT] LIKE … [ESCAPE …], [NOT] BETWEEN, IS [NOT] NULL,
//! + - * /, string, number and boolean literals, header fields (JMSType, JMSCorrelationID, …) and
//! properties. Three-valued logic: a missing property is NULL (unknown), and only TRUE selects.

#[derive(Clone, Debug, PartialEq)]
pub enum V {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum E {
    Lit(V),
    Id(String),
    Neg(Box<E>),
    Not(Box<E>),
    And(Box<E>, Box<E>),
    Or(Box<E>, Box<E>),
    /// = <> < > <= >= + - * /
    Bin(&'static str, Box<E>, Box<E>),
    Between(Box<E>, Box<E>, Box<E>, bool),
    In(Box<E>, Vec<String>, bool),
    Like(Box<E>, Vec<char>, Option<char>, bool),
    IsNull(Box<E>, bool),
}

#[derive(Clone, Debug, PartialEq)]
enum T {
    Id(String),
    Kw(&'static str),
    Str(String),
    Num(V),
    Op(&'static str),
}

const KW: [&str; 11] = ["AND", "OR", "NOT", "BETWEEN", "LIKE", "ESCAPE", "IN", "IS", "NULL", "TRUE", "FALSE"];

fn lex(s: &str) -> Result<Vec<T>, String> {
    let c: Vec<char> = s.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '\'' {
            let mut v = String::new();
            i += 1;
            loop {
                match c.get(i) {
                    None => return Err("unterminated string literal".into()),
                    Some('\'') if c.get(i + 1) == Some(&'\'') => {
                        v.push('\'');
                        i += 2;
                    }
                    Some('\'') => {
                        i += 1;
                        break;
                    }
                    Some(x) => {
                        v.push(*x);
                        i += 1;
                    }
                }
            }
            out.push(T::Str(v));
        } else if ch.is_ascii_digit() || (ch == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            if ch == '0' && matches!(c.get(i + 1), Some('x' | 'X')) {
                i += 2;
                while c.get(i).is_some_and(|d| d.is_ascii_hexdigit()) {
                    i += 1;
                }
                let h: String = c[st + 2..i].iter().collect();
                out.push(T::Num(V::Int(i64::from_str_radix(&h, 16).map_err(|_| format!("bad hex number 0x{h}"))?)));
            } else {
                let mut float = false;
                while let Some(&d) = c.get(i) {
                    if d.is_ascii_digit() {
                        i += 1;
                    } else if d == '.' && !float {
                        float = true;
                        i += 1;
                    } else if (d == 'e' || d == 'E') && c.get(i + 1).is_some_and(|x| x.is_ascii_digit() || *x == '+' || *x == '-') {
                        float = true;
                        i += 2;
                    } else {
                        break;
                    }
                }
                let n: String = c[st..i].iter().collect();
                if matches!(c.get(i), Some('l' | 'L')) {
                    i += 1;
                } else if matches!(c.get(i), Some('f' | 'F' | 'd' | 'D')) {
                    i += 1;
                    float = true;
                }
                out.push(T::Num(if float {
                    V::Float(n.parse().map_err(|_| format!("bad number {n}"))?)
                } else {
                    n.parse().map(V::Int).or_else(|_| n.parse().map(V::Float)).map_err(|_| format!("bad number {n}"))?
                }));
            }
        } else if ch.is_alphabetic() || ch == '_' || ch == '$' {
            let st = i;
            while c.get(i).is_some_and(|x| x.is_alphanumeric() || *x == '_' || *x == '$' || *x == '.') {
                i += 1;
            }
            let w: String = c[st..i].iter().collect();
            out.push(match KW.iter().find(|k| k.eq_ignore_ascii_case(&w)) {
                Some(k) => T::Kw(k),
                None => T::Id(w),
            });
        } else {
            let two: String = c[i..(i + 2).min(c.len())].iter().collect();
            let op = ["<>", "<=", ">="].into_iter().find(|o| *o == two).or_else(|| ["=", "<", ">", "+", "-", "*", "/", "(", ")", ","].into_iter().find(|o| o.starts_with(ch)));
            match op {
                Some(o) => {
                    i += o.len();
                    out.push(T::Op(o));
                }
                None => return Err(format!("unexpected character '{ch}'")),
            }
        }
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
    fn eat(&mut self, t: &T) -> bool {
        let ok = self.peek() == Some(t);
        self.i += ok as usize;
        ok
    }
    fn kw(&mut self, k: &'static str) -> bool {
        self.eat(&T::Kw(k))
    }
    fn need(&mut self, t: T) -> Result<(), String> {
        if self.eat(&t) {
            Ok(())
        } else {
            Err(format!("expected {t:?}, found {:?}", self.peek()))
        }
    }
    fn str(&mut self) -> Result<String, String> {
        match self.t.get(self.i).cloned() {
            Some(T::Str(s)) => {
                self.i += 1;
                Ok(s)
            }
            t => Err(format!("expected a string literal, found {t:?}")),
        }
    }
    fn or(&mut self) -> Result<E, String> {
        let mut l = self.and()?;
        while self.kw("OR") {
            l = E::Or(Box::new(l), Box::new(self.and()?));
        }
        Ok(l)
    }
    fn and(&mut self) -> Result<E, String> {
        let mut l = self.not()?;
        while self.kw("AND") {
            l = E::And(Box::new(l), Box::new(self.not()?));
        }
        Ok(l)
    }
    fn not(&mut self) -> Result<E, String> {
        if self.kw("NOT") {
            return Ok(E::Not(Box::new(self.not()?)));
        }
        self.cmp()
    }
    fn cmp(&mut self) -> Result<E, String> {
        let l = self.add()?;
        if let Some(T::Op(o @ ("=" | "<>" | "<" | ">" | "<=" | ">="))) = self.peek().cloned() {
            self.i += 1;
            return Ok(E::Bin(o, Box::new(l), Box::new(self.add()?)));
        }
        if self.kw("IS") {
            let not = self.kw("NOT");
            self.need(T::Kw("NULL"))?;
            return Ok(E::IsNull(Box::new(l), not));
        }
        let not = self.kw("NOT");
        if self.kw("BETWEEN") {
            let lo = self.add()?;
            self.need(T::Kw("AND"))?;
            return Ok(E::Between(Box::new(l), Box::new(lo), Box::new(self.add()?), not));
        }
        if self.kw("IN") {
            self.need(T::Op("("))?;
            let mut list = vec![self.str()?];
            while self.eat(&T::Op(",")) {
                list.push(self.str()?);
            }
            self.need(T::Op(")"))?;
            return Ok(E::In(Box::new(l), list, not));
        }
        if self.kw("LIKE") {
            let pat = self.str()?.chars().collect();
            let esc = if self.kw("ESCAPE") {
                let e = self.str()?;
                let mut ch = e.chars();
                match (ch.next(), ch.next()) {
                    (Some(c), None) => Some(c),
                    _ => return Err("ESCAPE takes one character".into()),
                }
            } else {
                None
            };
            return Ok(E::Like(Box::new(l), pat, esc, not));
        }
        if not {
            return Err("NOT here must be followed by BETWEEN, IN or LIKE".into());
        }
        Ok(l)
    }
    fn add(&mut self) -> Result<E, String> {
        let mut l = self.mul()?;
        while let Some(T::Op(o @ ("+" | "-"))) = self.peek().cloned() {
            self.i += 1;
            l = E::Bin(o, Box::new(l), Box::new(self.mul()?));
        }
        Ok(l)
    }
    fn mul(&mut self) -> Result<E, String> {
        let mut l = self.unary()?;
        while let Some(T::Op(o @ ("*" | "/"))) = self.peek().cloned() {
            self.i += 1;
            l = E::Bin(o, Box::new(l), Box::new(self.unary()?));
        }
        Ok(l)
    }
    fn unary(&mut self) -> Result<E, String> {
        if self.eat(&T::Op("-")) {
            return Ok(match self.unary()? {
                E::Lit(V::Int(n)) => E::Lit(V::Int(-n)),
                E::Lit(V::Float(n)) => E::Lit(V::Float(-n)),
                e => E::Neg(Box::new(e)),
            });
        }
        if self.eat(&T::Op("+")) {
            return self.unary();
        }
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        Ok(match t {
            Some(T::Op("(")) => {
                let e = self.or()?;
                self.need(T::Op(")"))?;
                e
            }
            Some(T::Str(s)) => E::Lit(V::Str(s)),
            Some(T::Num(n)) => E::Lit(n),
            Some(T::Kw("TRUE")) => E::Lit(V::Bool(true)),
            Some(T::Kw("FALSE")) => E::Lit(V::Bool(false)),
            Some(T::Kw("NULL")) => E::Lit(V::Null),
            Some(T::Id(s)) => E::Id(s),
            t => return Err(format!("unexpected {}", t.map(|t| format!("{t:?}")).unwrap_or("end of selector".into()))),
        })
    }
}

/// Parses a selector; the error is what goes into the InvalidSelectorException.
pub fn parse(s: &str) -> Result<E, String> {
    let mut p = P { t: lex(s)?, i: 0 };
    let e = p.or()?;
    match p.peek() {
        None => Ok(e),
        Some(t) => Err(format!("unexpected {t:?} after the expression")),
    }
}

fn num(v: &V) -> Option<f64> {
    match v {
        V::Int(n) => Some(*n as f64),
        V::Float(f) => Some(*f),
        _ => None,
    }
}

fn truth(v: &V) -> Option<bool> {
    match v {
        V::Bool(b) => Some(*b),
        _ => None,
    }
}

fn b3(b: Option<bool>) -> V {
    b.map(V::Bool).unwrap_or(V::Null)
}

fn like(s: &[char], p: &[char], esc: Option<char>) -> bool {
    // Tokens: Some(c) a literal char, None '_', and '%' as a separate marker.
    let mut toks: Vec<(bool, Option<char>)> = vec![];
    let mut i = 0;
    while i < p.len() {
        if Some(p[i]) == esc && i + 1 < p.len() {
            toks.push((false, Some(p[i + 1])));
            i += 2;
            continue;
        }
        toks.push(match p[i] {
            '%' => (true, None),
            '_' => (false, None),
            c => (false, Some(c)),
        });
        i += 1;
    }
    // dp[j]: the first i chars of s match the first j tokens.
    let mut dp = vec![false; toks.len() + 1];
    dp[0] = true;
    for j in 0..toks.len() {
        dp[j + 1] = dp[j] && toks[j].0;
    }
    for ch in s {
        let mut nx = vec![false; toks.len() + 1];
        for j in 0..toks.len() {
            nx[j + 1] = match toks[j] {
                (true, _) => nx[j] || dp[j + 1],
                (false, None) => dp[j],
                (false, Some(c)) => dp[j] && c == *ch,
            };
        }
        dp = nx;
    }
    dp[toks.len()]
}

impl E {
    pub fn eval(&self, get: &dyn Fn(&str) -> V) -> V {
        match self {
            E::Lit(v) => v.clone(),
            E::Id(n) => get(n),
            E::Neg(e) => match e.eval(get) {
                V::Int(n) => V::Int(-n),
                V::Float(f) => V::Float(-f),
                _ => V::Null,
            },
            E::Not(e) => b3(truth(&e.eval(get)).map(|b| !b)),
            E::And(a, b) => match (truth(&a.eval(get)), truth(&b.eval(get))) {
                (Some(false), _) | (_, Some(false)) => V::Bool(false),
                (Some(true), Some(true)) => V::Bool(true),
                _ => V::Null,
            },
            E::Or(a, b) => match (truth(&a.eval(get)), truth(&b.eval(get))) {
                (Some(true), _) | (_, Some(true)) => V::Bool(true),
                (Some(false), Some(false)) => V::Bool(false),
                _ => V::Null,
            },
            E::Bin(op, a, b) => {
                let (l, r) = (a.eval(get), b.eval(get));
                if l == V::Null || r == V::Null {
                    return V::Null;
                }
                match *op {
                    "+" | "-" | "*" | "/" => match (&l, &r) {
                        (V::Str(x), V::Str(y)) if *op == "+" => V::Str(format!("{x}{y}")),
                        (V::Int(x), V::Int(y)) => match *op {
                            "+" => x.checked_add(*y).map(V::Int).unwrap_or(V::Float(*x as f64 + *y as f64)),
                            "-" => x.checked_sub(*y).map(V::Int).unwrap_or(V::Float(*x as f64 - *y as f64)),
                            "*" => x.checked_mul(*y).map(V::Int).unwrap_or(V::Float(*x as f64 * *y as f64)),
                            _ => x.checked_div(*y).map(V::Int).unwrap_or(V::Null),
                        },
                        _ => match (num(&l), num(&r)) {
                            (Some(x), Some(y)) => V::Float(match *op {
                                "+" => x + y,
                                "-" => x - y,
                                "*" => x * y,
                                _ => x / y,
                            }),
                            _ => V::Null,
                        },
                    },
                    _ => {
                        let ord = match (&l, &r) {
                            (V::Str(x), V::Str(y)) => Some(x.cmp(y)),
                            (V::Bool(x), V::Bool(y)) if matches!(*op, "=" | "<>") => Some(x.cmp(y)),
                            _ => match (num(&l), num(&r)) {
                                (Some(x), Some(y)) => x.partial_cmp(&y),
                                // Values of different types never compare equal (nor unequal).
                                _ => return V::Bool(false),
                            },
                        };
                        let Some(o) = ord else { return V::Bool(false) };
                        V::Bool(match *op {
                            "=" => o.is_eq(),
                            "<>" => o.is_ne(),
                            "<" => o.is_lt(),
                            ">" => o.is_gt(),
                            "<=" => o.is_le(),
                            _ => o.is_ge(),
                        })
                    }
                }
            }
            E::Between(e, lo, hi, not) => {
                let ge = E::Bin(">=", e.clone(), lo.clone()).eval(get);
                let le = E::Bin("<=", e.clone(), hi.clone()).eval(get);
                let v = E::And(Box::new(E::Lit(ge)), Box::new(E::Lit(le))).eval(get);
                if *not { b3(truth(&v).map(|b| !b)) } else { v }
            }
            E::In(e, list, not) => match e.eval(get) {
                V::Str(s) => V::Bool(list.contains(&s) != *not),
                V::Null => V::Null,
                _ => V::Bool(false),
            },
            E::Like(e, pat, esc, not) => match e.eval(get) {
                V::Str(s) => V::Bool(like(&s.chars().collect::<Vec<_>>(), pat, *esc) != *not),
                V::Null => V::Null,
                _ => V::Bool(false),
            },
            E::IsNull(e, not) => V::Bool((e.eval(get) == V::Null) != *not),
        }
    }

    /// Only TRUE selects; FALSE and unknown do not.
    pub fn matches(&self, get: &dyn Fn(&str) -> V) -> bool {
        self.eval(get) == V::Bool(true)
    }
}

#[cfg(test)]
mod t {
    use super::*;

    fn sel(s: &str) -> bool {
        let get = |k: &str| match k {
            "region" => V::Str("north".into()),
            "amount" => V::Int(250),
            "rate" => V::Float(1.5),
            "vip" => V::Bool(true),
            "JMSType" => V::Str("order.created".into()),
            "JMSPriority" => V::Int(7),
            "code" => V::Str("A_1%".into()),
            _ => V::Null,
        };
        parse(s).unwrap_or_else(|e| panic!("{s}: {e}")).matches(&get)
    }

    #[test]
    fn selects() {
        for s in [
            "region = 'north'",
            "region <> 'south' AND amount > 100",
            "amount >= 250 AND amount <= 250 AND amount < 251",
            "amount BETWEEN 200 AND 300",
            "amount NOT BETWEEN 1 AND 10",
            "region IN ('north', 'east')",
            "region NOT IN ('south')",
            "region LIKE 'no%'",
            "region LIKE '_orth'",
            "code LIKE 'A!_1!%' ESCAPE '!'",
            "region NOT LIKE 's%'",
            "missing IS NULL",
            "region IS NOT NULL",
            "amount * 2 - 100 = 400",
            "amount / 2 = 125 AND -amount < 0 AND +amount > 0",
            "rate * 2 = 3.0 AND rate > 1",
            "vip = TRUE AND NOT vip = FALSE",
            "JMSType = 'order.created' AND JMSPriority > 4",
            "(region = 'south' OR amount > 200) AND (vip)",
            "missing = 'x' OR region = 'north'",
            "NOT (missing = 'x') OR TRUE",
            "amount = 0xFA AND 2.5e2 = amount",
            "region = 'it''s' OR region = 'north'",
            "'a' + 'b' = 'ab'",
            "region = 'north' and amount > 1 or false",
        ] {
            assert!(sel(s), "{s} should select");
        }
        for s in [
            "region = 'south'",
            "missing = 'x'",
            "NOT missing = 'x'",
            "missing > 1",
            "missing IN ('x')",
            "missing NOT IN ('x')",
            "region = 1",
            "amount = '250'",
            "amount BETWEEN 1 AND 10",
            "code LIKE 'A_1%' ESCAPE '!' AND code LIKE 'B%'",
            "vip AND missing",
            "amount / 0 = 1",
        ] {
            assert!(!sel(s), "{s} should not select");
        }
    }

    #[test]
    fn rejects() {
        for s in ["", "region = = 'x'", "region =", "region IN ()", "region IN (1)", "(region = 'x'", "region LIKE x", "'unterminated", "region # 1", "a NOT b", "x LIKE 'a' ESCAPE 'ab'"] {
            assert!(parse(s).is_err(), "{s:?} should be invalid");
        }
    }
}
