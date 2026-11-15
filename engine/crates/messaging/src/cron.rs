//! Standard 5-field cron (minute hour day-of-month month day-of-week) for AMQ_SCHEDULED_CRON:
//! `*`, lists, ranges, steps, month and day names, Sunday as 0 or 7. When both day fields are
//! restricted, either one matching is enough (as Vixie cron and ActiveMQ's CronParser do). UTC.

#[derive(Clone, Debug, PartialEq)]
pub struct Cron {
    min: Vec<bool>,
    hour: Vec<bool>,
    dom: Vec<bool>,
    month: Vec<bool>,
    dow: Vec<bool>,
    dom_any: bool,
    dow_any: bool,
}

const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
const DAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

fn num(s: &str, names: &[&str], base: u32) -> Result<u32, String> {
    if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
        return Ok(i as u32 + base);
    }
    s.parse().map_err(|_| format!("“{s}” is not a number"))
}

/// One field into a table of allowed values lo..=hi.
fn field(s: &str, lo: u32, hi: u32, names: &[&str], base: u32) -> Result<(Vec<bool>, bool), String> {
    let mut t = vec![false; hi as usize + 1];
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (r, st.parse::<u32>().ok().filter(|x| *x > 0).ok_or_else(|| format!("bad step in “{part}”"))?),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a, names, base)?, num(b, names, base)?)
        } else {
            let a = num(range, names, base)?;
            (a, if step > 1 { hi } else { a })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("“{part}” is outside {lo}-{hi}"));
        }
        for v in (a..=b).step_by(step as usize) {
            t[v as usize] = true;
        }
    }
    Ok((t, s == "*"))
}

pub fn parse(s: &str) -> Result<Cron, String> {
    let f: Vec<&str> = s.split_whitespace().collect();
    if f.len() != 5 {
        return Err(format!("“{s}”: a cron entry has 5 fields (minute hour day-of-month month day-of-week)"));
    }
    let (min, _) = field(f[0], 0, 59, &[], 0)?;
    let (hour, _) = field(f[1], 0, 23, &[], 0)?;
    let (dom, dom_any) = field(f[2], 1, 31, &[], 0)?;
    let (month, _) = field(f[3], 1, 12, &MONTHS, 1)?;
    let (mut dow, dow_any) = field(f[4], 0, 7, &DAYS, 0)?;
    if dow[7] {
        dow[0] = true;
    }
    Ok(Cron { min, hour, dom, month, dow, dom_any, dow_any })
}

/// Days since 1970-01-01 → (year, month 1-12, day 1-31) (Howard Hinnant's civil_from_days).
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

impl Cron {
    fn day(&self, days: i64) -> bool {
        let (_, m, d) = civil(days);
        let wd = (days + 4).rem_euclid(7) as usize; // 1970-01-01 was a Thursday
        let (a, b) = (self.dom[d as usize], self.dow[wd]);
        self.month[m as usize]
            && match (self.dom_any, self.dow_any) {
                (true, true) => true,
                (true, false) => b,
                (false, true) => a,
                (false, false) => a || b,
            }
    }

    /// The first time (ms) strictly after `after` (ms) that matches; None if none within 5 years.
    pub fn next(&self, after: i64) -> Option<i64> {
        let mut m = after.div_euclid(60_000) + 1;
        let end = m + 5 * 366 * 1440;
        while m < end {
            let days = m.div_euclid(1440);
            if !self.day(days) {
                m = (days + 1) * 1440;
                continue;
            }
            let mi = m.rem_euclid(1440);
            if !self.hour[(mi / 60) as usize] {
                m = days * 1440 + (mi / 60 + 1) * 60;
                continue;
            }
            if self.min[(mi % 60) as usize] {
                return Some(m * 60_000);
            }
            m += 1;
        }
        None
    }
}

#[cfg(test)]
mod t {
    use super::*;

    // 2026-11-15 10:07:30 UTC, a Sunday.
    const NOW: i64 = 1_794_737_250_000;

    fn at(s: &str) -> i64 {
        parse(s).unwrap().next(NOW).unwrap()
    }

    #[test]
    fn next_times() {
        assert_eq!(civil(NOW / 86_400_000), (2026, 11, 15));
        assert_eq!(at("* * * * *"), NOW - 30_000 + 60_000, "the next minute");
        assert_eq!(at("*/5 * * * *"), NOW - 7 * 60_000 - 30_000 + 10 * 60_000, "10:10");
        assert_eq!(at("0 12 * * *"), NOW - (7 * 60 + 30) * 1000 + 120 * 60_000, "12:00 today");
        assert_eq!(at("0 9 * * *"), NOW + 86_400_000 - (67 * 60 + 30) * 1000, "09:00 tomorrow");
        assert_eq!(at("30 8 * * MON"), NOW + 86_400_000 - (97 * 60 + 30) * 1000, "Monday 08:30");
        assert_eq!(at("0 0 1 JAN *"), 1_798_761_600_000, "2027-01-01");
        assert_eq!(at("0 0 * * 7"), NOW + 7 * 86_400_000 - (10 * 3600 + 7 * 60 + 30) * 1000, "next Sunday 00:00 (7 = Sunday)");
        // Both day fields restricted: either matches (the 16th is a Monday, the 20th a Friday).
        assert_eq!(at("0 0 20 * 1"), NOW + 86_400_000 - (10 * 3600 + 7 * 60 + 30) * 1000);
        assert_eq!(at("15,45 10-11 * * *"), NOW + (7 * 60 + 30) * 1000);
        assert_eq!(parse("0 0 30 2 *").unwrap().next(NOW), None, "February 30 never comes");
    }

    #[test]
    fn invalid() {
        for s in ["", "* * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "* * * 13 *", "* * * * 8", "*/0 * * * *", "a * * * *", "5-1 * * * *"] {
            assert!(parse(s).is_err(), "{s:?}");
        }
    }
}
