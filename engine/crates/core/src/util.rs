use rand::Rng;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// Random id with a readable prefix: "msg_3f2c9e1a".
pub fn id(prefix: &str) -> String {
    format!("{prefix}_{:08x}", rand::thread_rng().gen::<u32>())
}

/// "30s", "500ms", "2h", "7d", "1.5m"; a bare number is seconds.
pub fn parse_ms(s: &str) -> Option<u64> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: f64 = n.parse().ok()?;
    let f = match unit.trim() {
        "" | "s" | "sec" | "secs" | "seconds" => 1000.0,
        "ms" | "milliseconds" => 1.0,
        "m" | "min" | "mins" | "minutes" => 60_000.0,
        "h" | "hours" => 3_600_000.0,
        "d" | "days" => 86_400_000.0,
        _ => return None,
    };
    Some((n * f) as u64)
}

/// Inverse of parse_ms for YAML and the screen: 1800000 → "30m".
pub fn fmt_ms(ms: u64) -> String {
    for (u, f) in [("d", 86_400_000), ("h", 3_600_000), ("m", 60_000), ("s", 1000)] {
        if ms >= f && ms % f == 0 {
            return format!("{}{u}", ms / f);
        }
    }
    format!("{ms}ms")
}

#[cfg(test)]
mod t {
    #[test]
    fn durations() {
        assert_eq!(super::parse_ms("30m"), Some(1_800_000));
        assert_eq!(super::parse_ms("500ms"), Some(500));
        assert_eq!(super::parse_ms("2"), Some(2000));
        assert_eq!(super::fmt_ms(1_800_000), "30m");
        assert_eq!(super::parse_ms("x"), None);
    }
}

/// A secret value: literal, "env:NAME", or "secret://name" (data/secrets.yaml until the Secrets
/// service exists; the reference stays the same afterwards).
pub fn resolve_secret(v: &str, data_dir: &std::path::Path) -> String {
    if let Some(var) = v.strip_prefix("env:") {
        return std::env::var(var).unwrap_or_default();
    }
    if let Some(name) = v.strip_prefix("secret://") {
        let m: std::collections::BTreeMap<String, String> =
            std::fs::read_to_string(data_dir.join("secrets.yaml")).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
        return m.get(name).cloned().unwrap_or_default();
    }
    v.to_string()
}

/// Writes a seed secret into the environment's secrets.yaml (keys Stuntman makes, e.g. storage account
/// keys). Returns the secret://name reference.
pub fn set_secret(data_dir: &std::path::Path, name: &str, value: &str) -> std::io::Result<String> {
    let p = data_dir.join("secrets.yaml");
    let mut m: std::collections::BTreeMap<String, String> =
        std::fs::read_to_string(&p).ok().and_then(|t| serde_yaml::from_str(&t).ok()).unwrap_or_default();
    m.insert(name.to_string(), value.to_string());
    std::fs::write(&p, serde_yaml::to_string(&m).unwrap_or_default())?;
    Ok(format!("secret://{name}"))
}

/// A random UUID v4 in its usual text form.
pub fn uuid() -> String {
    let b: [u8; 16] = rand::thread_rng().gen();
    let h = |r: std::ops::Range<usize>| b[r].iter().map(|x| format!("{x:02x}")).collect::<String>();
    let v = format!("4{}", &h(6..8)[1..]);
    let var = format!("{:x}{}", 8 + (b[8] & 0x3), &h(8..10)[1..]);
    format!("{}-{}-{}-{}-{}", h(0..4), h(4..6), v, var, h(10..16))
}
