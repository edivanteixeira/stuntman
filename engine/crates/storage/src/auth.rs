//! Signatures the real providers check, checked the same way: Azure SharedKey and SAS (service and
//! account), S3 SigV4 in the header and presigned in the query. With "accept any key" on, only a
//! link's expiry, permissions and IPs count; with it off, a wrong signature gets 403 like the cloud.

use std::collections::BTreeMap;

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use hmac::{Hmac, Mac};
use sha2::Sha256;

type H = Hmac<Sha256>;

pub fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    let mut m = H::new_from_slice(key).expect("hmac key");
    m.update(msg);
    m.finalize().into_bytes().to_vec()
}

// ---------------------------------------------------------------- time

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "2026-11-13T14:33:00Z", "2026-11-13T14:33:00.1234567Z", "2026-11-13" or "20261113T143300Z" → ms.
pub fn iso_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 8 {
        return None;
    }
    let n = |a: usize, b: usize| digits.get(a..b).and_then(|x| x.parse::<i64>().ok());
    let (y, mo, d) = (n(0, 4)?, n(4, 6)?, n(6, 8)?);
    let (h, mi, se) = (n(8, 10).unwrap_or(0), n(10, 12).unwrap_or(0), n(12, 14).unwrap_or(0));
    Some(((days_from_civil(y, mo, d) * 86_400) + h * 3600 + mi * 60 + se) * 1000)
}

pub fn ms_iso(ms: i64) -> String {
    let (s, days) = ((ms / 1000).rem_euclid(86_400), (ms / 1000).div_euclid(86_400));
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

/// "Thu, 13 Nov 2026 14:33:00 GMT"
pub fn rfc1123(ms: i64) -> String {
    let (s, days) = ((ms / 1000).rem_euclid(86_400), (ms / 1000).div_euclid(86_400));
    let (y, m, d) = civil(days);
    const W: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const M: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{}, {d:02} {} {y} {:02}:{:02}:{:02} GMT", W[days.rem_euclid(7) as usize], M[(m - 1) as usize], s / 3600, s % 3600 / 60, s % 60)
}

// ---------------------------------------------------------------- query and encoding

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() + 0 && i + 2 <= b.len() - 1 {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Path segments are not form-encoded: "+" stays "+".
pub fn decode_path(s: &str) -> String {
    decode(&s.replace('+', "%2B"))
}

/// Query pairs, decoded, in order.
pub fn query(q: &str) -> Vec<(String, String)> {
    q.split('&').filter(|x| !x.is_empty()).map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (decode(k), decode(v))
    }).collect()
}

/// AWS URI encoding: unreserved stay, the rest %XX (upper); "/" kept when `path`.
pub fn aws_encode(s: &str, path: bool) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || (path && b == b'/') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

// ---------------------------------------------------------------- Azure

/// Azure SharedKey: "SharedKey account:sig". `path` is the request path exactly as sent.
pub fn azure_shared_key(method: &str, path: &str, q: &[(String, String)], h: &BTreeMap<String, String>, account: &str, key_b64: &str) -> String {
    let g = |k: &str| h.get(k).cloned().unwrap_or_default();
    let len = g("content-length");
    let mut s = format!(
        "{method}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        g("content-encoding"), g("content-language"), if len == "0" { String::new() } else { len }, g("content-md5"), g("content-type"),
        g("date"), g("if-modified-since"), g("if-match"), g("if-none-match"), g("if-unmodified-since"), g("range")
    );
    for (k, v) in h.iter().filter(|(k, _)| k.starts_with("x-ms-")) {
        s.push_str(&format!("{k}:{}\n", v.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    s.push_str(&format!("/{account}{path}"));
    let mut qs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, v) in q {
        qs.entry(k.to_lowercase()).or_default().push(v.clone());
    }
    for (k, mut v) in qs {
        v.sort();
        s.push_str(&format!("\n{k}:{}", v.join(",")));
    }
    let key = B64.decode(key_b64).unwrap_or_default();
    B64.encode(hmac(&key, s.as_bytes()))
}

/// The signature a SAS should carry, for service SAS (sr=b/c) or account SAS (ss=…).
pub fn azure_sas(q: &BTreeMap<String, String>, account: &str, container: &str, blob: &str, key_b64: &str) -> String {
    let g = |k: &str| q.get(k).cloned().unwrap_or_default();
    let sv = g("sv");
    let s = if q.contains_key("ss") {
        let mut s = format!("{account}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{sv}\n", g("sp"), g("ss"), g("srt"), g("st"), g("se"), g("sip"), g("spr"));
        if sv.as_str() >= "2020-12-06" {
            s.push_str(&format!("{}\n", g("ses")));
        }
        s
    } else {
        let res = if g("sr") == "c" { format!("/blob/{account}/{container}") } else { format!("/blob/{account}/{container}/{blob}") };
        let mut parts = vec![g("sp"), g("st"), g("se"), res, g("si"), g("sip"), g("spr"), sv.clone(), g("sr"), g("snapshot")];
        if sv.as_str() >= "2020-12-06" {
            parts.push(g("ses"));
        }
        for k in ["rscc", "rscd", "rsce", "rscl", "rsct"] {
            parts.push(g(k));
        }
        parts.join("\n")
    };
    let key = B64.decode(key_b64).unwrap_or_default();
    B64.encode(hmac(&key, s.as_bytes()))
}

/// A service SAS Stuntman makes (create_link): the query string, signed with the account key.
pub fn make_azure_sas(account: &str, container: &str, blob: &str, sp: &str, start: i64, expiry: i64, ip: &str, key_b64: &str) -> String {
    let mut q = BTreeMap::new();
    q.insert("sv".to_string(), "2024-11-04".to_string());
    q.insert("st".into(), ms_iso(start));
    q.insert("se".into(), ms_iso(expiry));
    q.insert("sr".into(), "b".into());
    q.insert("sp".into(), sp.into());
    q.insert("spr".into(), "https,http".into());
    if !ip.is_empty() {
        q.insert("sip".into(), ip.into());
    }
    let sig = azure_sas(&q, account, container, blob, key_b64);
    q.insert("sig".into(), sig);
    ["sv", "st", "se", "sr", "sp", "sip", "spr", "sig"].iter().filter_map(|k| q.get(*k).map(|v| format!("{k}={}", aws_encode(v, false)))).collect::<Vec<_>>().join("&")
}

// ---------------------------------------------------------------- S3 SigV4

fn signing_key(secret: &str, date: &str, region: &str) -> Vec<u8> {
    let k = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k = hmac(&k, region.as_bytes());
    let k = hmac(&k, b"s3");
    hmac(&k, b"aws4_request")
}

pub struct V4<'a> {
    pub method: &'a str,
    /// Path as sent (still percent-encoded).
    pub raw_path: &'a str,
    pub query: &'a [(String, String)],
    pub headers: &'a BTreeMap<String, String>,
    pub signed_headers: &'a str,
    pub payload_hash: &'a str,
    pub amz_date: &'a str,
    /// date/region/s3/aws4_request
    pub scope: &'a str,
}

pub fn sigv4(v: &V4, secret: &str) -> String {
    let uri = aws_encode(&decode_path(v.raw_path), true);
    let mut qs: Vec<(String, String)> = v.query.iter().filter(|(k, _)| k != "X-Amz-Signature").map(|(k, x)| (aws_encode(k, false), aws_encode(x, false))).collect();
    qs.sort();
    let canon_q = qs.iter().map(|(k, x)| format!("{k}={x}")).collect::<Vec<_>>().join("&");
    let mut canon_h = String::new();
    for h in v.signed_headers.split(';') {
        canon_h.push_str(&format!("{h}:{}\n", v.headers.get(h).map(|x| x.split_whitespace().collect::<Vec<_>>().join(" ")).unwrap_or_default()));
    }
    let creq = format!("{}\n{uri}\n{canon_q}\n{canon_h}\n{}\n{}", v.method, v.signed_headers, v.payload_hash);
    let sts = format!("AWS4-HMAC-SHA256\n{}\n{}\n{}", v.amz_date, v.scope, crate::store::sha256_hex(creq.as_bytes()));
    let mut parts = v.scope.split('/');
    let (date, region) = (parts.next().unwrap_or(""), parts.next().unwrap_or("auto"));
    hex::encode(hmac(&signing_key(secret, date, region), sts.as_bytes()))
}

/// A presigned GET/PUT URL query Stuntman makes (create_link on an S3 account).
pub fn make_presigned(method: &str, host: &str, raw_path: &str, key_id: &str, secret: &str, now: i64, secs: i64) -> String {
    let amz = ms_iso(now).replace(['-', ':'], "");
    let date = &amz[..8];
    let scope = format!("{date}/auto/s3/aws4_request");
    let mut q = vec![
        ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential".into(), format!("{key_id}/{scope}")),
        ("X-Amz-Date".into(), amz.clone()),
        ("X-Amz-Expires".into(), secs.to_string()),
        ("X-Amz-SignedHeaders".into(), "host".into()),
    ];
    let mut h = BTreeMap::new();
    h.insert("host".to_string(), host.to_string());
    let sig = sigv4(&V4 { method, raw_path, query: &q, headers: &h, signed_headers: "host", payload_hash: "UNSIGNED-PAYLOAD", amz_date: &amz, scope: &scope }, secret);
    q.push(("X-Amz-Signature".into(), sig));
    q.iter().map(|(k, v)| format!("{k}={}", aws_encode(v, false))).collect::<Vec<_>>().join("&")
}

/// aws-chunked bodies (signed chunks or unsigned with trailers) → the plain bytes.
pub fn unchunk(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let Some(end) = b[i..].windows(2).position(|w| w == b"\r\n") else { break };
        let line = String::from_utf8_lossy(&b[i..i + end]).to_string();
        let size = usize::from_str_radix(line.split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        i += end + 2;
        if size == 0 {
            break;
        }
        let stop = (i + size).min(b.len());
        out.extend_from_slice(&b[i..stop]);
        i = stop + 2;
    }
    out
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn times() {
        let ms = iso_ms("2026-11-13T14:33:00Z").unwrap();
        assert_eq!(ms_iso(ms), "2026-11-13T14:33:00Z");
        assert_eq!(iso_ms("20261113T143300Z"), Some(ms));
        assert_eq!(rfc1123(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert!(rfc1123(ms).starts_with("Fri, 13 Nov 2026"));
    }

    /// The SigV4 example from the AWS documentation ("GET Object" with a Range header).
    #[test]
    fn sigv4_matches_aws_example() {
        let mut h = BTreeMap::new();
        h.insert("host".to_string(), "examplebucket.s3.amazonaws.com".to_string());
        h.insert("range".into(), "bytes=0-9".into());
        h.insert("x-amz-content-sha256".into(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into());
        h.insert("x-amz-date".into(), "20130524T000000Z".into());
        let sig = sigv4(
            &V4 { method: "GET", raw_path: "/test.txt", query: &[], headers: &h, signed_headers: "host;range;x-amz-content-sha256;x-amz-date",
                  payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", amz_date: "20130524T000000Z", scope: "20130524/us-east-1/s3/aws4_request" },
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        assert_eq!(sig, "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41");
    }

    #[test]
    fn chunks() {
        let b = b"5;chunk-signature=abc\r\nhello\r\n6;chunk-signature=d\r\n world\r\n0;chunk-signature=e\r\n\r\n";
        assert_eq!(unchunk(b), b"hello world");
        assert_eq!(unchunk(b"3\r\nabc\r\n0\r\nx-amz-checksum-crc32:AAAA\r\n\r\n"), b"abc");
    }

    #[test]
    fn sas_round_trip() {
        let key = B64.encode([7u8; 64]);
        let q = make_azure_sas("acme", "documents", "a b.pdf", "r", 0, 3_600_000, "", &key);
        let m: BTreeMap<String, String> = query(&q).into_iter().collect();
        assert_eq!(azure_sas(&m, "acme", "documents", "a b.pdf", &key), m["sig"]);
    }
}
