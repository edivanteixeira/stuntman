//! CBOR, as the AWS SDK for Java (v1 and v2) speaks to Kinesis by default
//! (application/x-amz-cbor-1.1). Decoded into the same JSON the JSON 1.1 protocol carries: byte
//! strings become base64 text, timestamps (epoch milliseconds in CBOR) become epoch seconds.
//! Encoding goes back: "Data" as a byte string, the *Timestamp fields as epoch milliseconds.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde_json::{Map, Number, Value};

fn is_time(k: &str) -> bool {
    k.ends_with("Timestamp")
}

pub fn decode(b: &[u8]) -> Result<Value, String> {
    let mut p = 0;
    let v = item(b, &mut p)?.ok_or("unexpected break")?;
    Ok(seconds(v, ""))
}

/// Timestamps arrive as milliseconds (Java's CBOR marshaller); the handlers read seconds.
fn seconds(v: Value, key: &str) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| { let v = seconds(v, &k); (k, v) }).collect()),
        Value::Array(a) => Value::Array(a.into_iter().map(|v| seconds(v, key)).collect()),
        Value::Number(n) if is_time(key) => Number::from_f64(n.as_f64().unwrap_or(0.0) / 1000.0).map(Value::Number).unwrap_or(Value::Null),
        o => o,
    }
}

fn uint(b: &[u8], p: &mut usize, info: u8) -> Result<u64, String> {
    let n = match info {
        0..=23 => return Ok(info as u64),
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => return Err(format!("bad CBOR length {info}")),
    };
    let s = b.get(*p..*p + n).ok_or("truncated CBOR")?;
    *p += n;
    Ok(s.iter().fold(0u64, |a, x| (a << 8) | *x as u64))
}

fn half(h: u16) -> f64 {
    let (e, m) = ((h >> 10) & 0x1f, (h & 0x3ff) as f64);
    let v = if e == 0 { m * 2f64.powi(-24) } else if e == 31 { if m == 0.0 { f64::INFINITY } else { f64::NAN } } else { (1.0 + m / 1024.0) * 2f64.powi(e as i32 - 15) };
    if h & 0x8000 != 0 { -v } else { v }
}

/// One item; None is the "break" of an indefinite-length container.
fn item(b: &[u8], p: &mut usize) -> Result<Option<Value>, String> {
    let ib = *b.get(*p).ok_or("truncated CBOR")?;
    *p += 1;
    let (major, info) = (ib >> 5, ib & 0x1f);
    if ib == 0xff {
        return Ok(None);
    }
    let indef = info == 31;
    Ok(Some(match major {
        0 => Value::from(uint(b, p, info)?),
        1 => Value::from(-1 - uint(b, p, info)? as i64),
        2 | 3 => {
            let mut raw = vec![];
            if indef {
                while b.get(*p) != Some(&0xff) {
                    let ib = *b.get(*p).ok_or("truncated CBOR")?;
                    *p += 1;
                    let n = uint(b, p, ib & 0x1f)? as usize;
                    raw.extend(b.get(*p..*p + n).ok_or("truncated CBOR")?);
                    *p += n;
                }
                *p += 1;
            } else {
                let n = uint(b, p, info)? as usize;
                raw.extend(b.get(*p..*p + n).ok_or("truncated CBOR")?);
                *p += n;
            }
            if major == 2 { Value::String(B64.encode(raw)) } else { Value::String(String::from_utf8(raw).map_err(|e| e.to_string())?) }
        }
        4 => {
            let mut a = vec![];
            if indef {
                while let Some(v) = item(b, p)? { a.push(v) }
            } else {
                for _ in 0..uint(b, p, info)? { a.push(item(b, p)?.ok_or("unexpected break")?) }
            }
            Value::Array(a)
        }
        5 => {
            let mut m = Map::new();
            let n = if indef { u64::MAX } else { uint(b, p, info)? };
            let mut i = 0;
            while i < n {
                let Some(k) = item(b, p)? else { break };
                let v = item(b, p)?.ok_or("unexpected break")?;
                m.insert(k.as_str().map(String::from).unwrap_or(k.to_string()), v);
                i += 1;
            }
            Value::Object(m)
        }
        6 => {
            uint(b, p, info)?;
            item(b, p)?.ok_or("unexpected break")?
        }
        _ => match info {
            20 => Value::Bool(false),
            21 => Value::Bool(true),
            22 | 23 => Value::Null,
            25 => { let h = uint(b, p, 25)? as u16; Number::from_f64(half(h)).map(Value::Number).unwrap_or(Value::Null) }
            26 => { let f = f32::from_bits(uint(b, p, 26)? as u32) as f64; Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null) }
            27 => Number::from_f64(f64::from_bits(uint(b, p, 27)?)).map(Value::Number).unwrap_or(Value::Null),
            x => return Err(format!("CBOR simple value {x}")),
        },
    }))
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xff => { out.push(m | 24); out.push(n as u8) }
        0x100..=0xffff => { out.push(m | 25); out.extend((n as u16).to_be_bytes()) }
        0x1_0000..=0xffff_ffff => { out.push(m | 26); out.extend((n as u32).to_be_bytes()) }
        _ => { out.push(m | 27); out.extend(n.to_be_bytes()) }
    }
}

pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = vec![];
    enc(&mut out, v, "");
    out
}

fn enc(out: &mut Vec<u8>, v: &Value, key: &str) {
    match v {
        Value::Null => out.push(0xf6),
        Value::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
        Value::Number(n) if is_time(key) => {
            let ms = (n.as_f64().unwrap_or(0.0) * 1000.0).round() as u64;
            head(out, 0, ms)
        }
        Value::Number(n) => match (n.as_u64(), n.as_i64()) {
            (Some(u), _) => head(out, 0, u),
            (_, Some(i)) => head(out, 1, (-1 - i) as u64),
            _ => { out.push(0xfb); out.extend(n.as_f64().unwrap_or(0.0).to_bits().to_be_bytes()) }
        },
        Value::String(s) if key == "Data" => {
            let raw = B64.decode(s).unwrap_or_default();
            head(out, 2, raw.len() as u64);
            out.extend(raw)
        }
        Value::String(s) => { head(out, 3, s.len() as u64); out.extend(s.as_bytes()) }
        Value::Array(a) => { head(out, 4, a.len() as u64); for x in a { enc(out, x, key) } }
        Value::Object(o) => {
            head(out, 5, o.len() as u64);
            for (k, x) in o {
                head(out, 3, k.len() as u64);
                out.extend(k.as_bytes());
                enc(out, x, k)
            }
        }
    }
}

#[cfg(test)]
mod t {
    use super::*;
    use serde_json::json;

    #[test]
    fn round_trip_with_blobs_and_millis() {
        let v = json!({"StreamName": "shipments", "Records": [{"Data": B64.encode(b"SHP-1042"), "PartitionKey": "SHP-1042", "ApproximateArrivalTimestamp": 1763190000.5}], "Limit": 10, "N": -3, "Ok": true});
        let b = encode(&v);
        assert_eq!(decode(&b).unwrap(), v);
        // An indefinite-length map (Jackson's writeStartObject) with a byte string.
        let mut b = vec![0xbf];
        b.extend([0x64]); b.extend(b"Data"); b.extend([0x43]); b.extend(b"abc");
        b.extend([0x69]); b.extend(b"Timestamp"); b.extend([0xc1, 0x1a]); b.extend(1_763_190_000u32.to_be_bytes());
        b.push(0xff);
        let d = decode(&b).unwrap();
        assert_eq!(d["Data"], json!(B64.encode(b"abc")));
        assert_eq!(d["Timestamp"].as_f64().unwrap(), 1_763_190.0);
        assert_eq!(half(0x3c00), 1.0);
    }
}
