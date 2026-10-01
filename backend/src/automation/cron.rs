//! Cron expressions for schedules: the standard five fields
//! (minute hour day-of-month month day-of-week) with `*`, lists, ranges,
//! steps and names (`mon`, `jan`), plus `@hourly`, `@daily` / `@midnight`,
//! `@weekly`, `@monthly`, `@yearly`. Day-of-month and day-of-week combine with
//! OR when both are restricted (like Vixie cron). Times are in a fixed UTC
//! offset (`UTC`, `+07:00`, `-05:30`).

use chrono::{DateTime, Datelike, Duration, FixedOffset, TimeZone, Timelike, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: u64,
    hour: u32,
    dom: u32,
    month: u16,
    dow: u8,
    dom_any: bool,
    dow_any: bool,
}

fn field(spec: &str, lo: u32, hi: u32, names: &[&str]) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let any = spec == "*" || spec == "?";
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| format!("bad step in `{part}`"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err(format!("step 0 in `{part}`"));
        }
        let value = |v: &str| -> Result<u32, String> {
            let v = v.to_ascii_lowercase();
            if let Some(i) = names.iter().position(|n| *n == v) {
                return Ok(i as u32 + lo);
            }
            v.parse::<u32>().map_err(|_| format!("`{v}` is not a number"))
        };
        let (a, b) = match range {
            "*" | "?" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (value(a)?, value(b)?),
                None => {
                    let v = value(r)?;
                    (v, if part.contains('/') { hi } else { v })
                }
            },
        };
        // Sunday may be written as 7.
        let (a, b) = if hi == 6 { (a.min(7), b.min(7)) } else { (a, b) };
        if a < lo || b > hi.max(if hi == 6 { 7 } else { hi }) || a > b {
            return Err(format!("`{part}` is out of range {lo}–{hi}"));
        }
        let mut v = a;
        while v <= b {
            bits |= 1 << (if hi == 6 && v == 7 { 0 } else { v });
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let e = expr.trim();
        let e = match e {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            other => other,
        };
        let f: Vec<&str> = e.split_whitespace().collect();
        if f.len() != 5 {
            return Err("a schedule has 5 fields: minute hour day month weekday (e.g. `0 2 * * *`)".into());
        }
        let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        let days = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
        let (minute, _) = field(f[0], 0, 59, &[])?;
        let (hour, _) = field(f[1], 0, 23, &[])?;
        let (dom, dom_any) = field(f[2], 1, 31, &[])?;
        let (month, _) = field(f[3], 1, 12, &months)?;
        let (dow, dow_any) = field(f[4], 0, 6, &days)?;
        Ok(Cron { minute, hour: hour as u32, dom: dom as u32, month: month as u16, dow: dow as u8, dom_any, dow_any })
    }

    fn matches(&self, t: &DateTime<FixedOffset>) -> bool {
        let day = self.day_ok(t);
        self.minute & (1 << t.minute()) != 0 && self.hour & (1 << t.hour()) != 0 && self.month & (1 << t.month()) != 0 && day
    }

    /// The first time strictly after `after` (at most ~4 years ahead).
    pub fn next(&self, after: DateTime<Utc>, tz: FixedOffset) -> Option<DateTime<Utc>> {
        let mut t = tz.from_utc_datetime(&after.naive_utc()).with_second(0)?.with_nanosecond(0)? + Duration::minutes(1);
        let end = t + Duration::days(366 * 4);
        while t < end {
            if self.month & (1 << t.month()) == 0 {
                // Jump to the first day of the next month.
                let (y, m) = if t.month() == 12 { (t.year() + 1, 1) } else { (t.year(), t.month() + 1) };
                t = tz.with_ymd_and_hms(y, m, 1, 0, 0, 0).single()?;
                continue;
            }
            if !self.day_ok(&t) {
                t = (t + Duration::days(1)).with_hour(0)?.with_minute(0)?;
                continue;
            }
            if self.hour & (1 << t.hour()) == 0 {
                t = (t + Duration::hours(1)).with_minute(0)?;
                continue;
            }
            if self.matches(&t) {
                return Some(t.with_timezone(&Utc));
            }
            t += Duration::minutes(1);
        }
        None
    }

    fn day_ok(&self, t: &DateTime<FixedOffset>) -> bool {
        let dom = self.dom & (1 << t.day()) != 0;
        let dow = self.dow & (1 << t.weekday().num_days_from_sunday()) != 0;
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }
}

/// `UTC`, `Z`, `+07:00`, `-0530`, `+7`.
pub fn parse_offset(s: &str) -> Result<FixedOffset, String> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("utc") || s == "Z" {
        return Ok(FixedOffset::east_opt(0).unwrap());
    }
    let bad = || format!("time zone `{s}`: use UTC or an offset like +07:00");
    let (sign, rest) = match s.as_bytes()[0] {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return Err(bad()),
    };
    let (h, m) = match rest.split_once(':') {
        Some((h, m)) => (h, m),
        None if rest.len() == 4 => (&rest[..2], &rest[2..]),
        None => (rest, "0"),
    };
    let (h, m): (i32, i32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 14 || m > 59 {
        return Err(bad());
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60)).ok_or_else(bad)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    fn next(expr: &str, after: &str, tz: &str) -> String {
        Cron::parse(expr).unwrap().next(at(after), parse_offset(tz).unwrap()).unwrap().to_rfc3339()
    }

    #[test]
    fn next_times() {
        assert_eq!(next("*/15 * * * *", "2026-10-01T10:07:30Z", "UTC"), "2026-10-01T10:15:00+00:00");
        assert_eq!(next("0 2 * * *", "2026-10-01T10:00:00Z", "UTC"), "2026-10-02T02:00:00+00:00");
        // 02:00 in UTC+7 is 19:00 UTC the day before.
        assert_eq!(next("0 2 * * *", "2026-10-01T10:00:00Z", "+07:00"), "2026-10-01T19:00:00+00:00");
        assert_eq!(next("30 9 * * mon-fri", "2026-10-02T10:00:00Z", "UTC"), "2026-10-05T09:30:00+00:00", "Fri after 9:30 → Monday");
        assert_eq!(next("0 0 1 * *", "2026-10-15T00:00:00Z", "UTC"), "2026-11-01T00:00:00+00:00");
        assert_eq!(next("0 0 29 2 *", "2026-03-01T00:00:00Z", "UTC"), "2028-02-29T00:00:00+00:00", "leap day");
        assert_eq!(next("@weekly", "2026-10-01T00:00:00Z", "UTC"), "2026-10-04T00:00:00+00:00", "Sunday");
        assert_eq!(next("0 12 1 * 7", "2026-10-02T00:00:00Z", "UTC"), "2026-10-04T12:00:00+00:00", "dom OR dow; 7 = Sunday");
        assert_eq!(next("0 * * * *", "2026-10-01T10:00:00Z", "UTC"), "2026-10-01T11:00:00+00:00", "strictly after");
    }

    #[test]
    fn bad_expressions() {
        for bad in ["", "* * * *", "60 * * * *", "* 24 * * *", "* * 0 * *", "*/0 * * * *", "x * * * *", "5-1 * * * *"] {
            assert!(Cron::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn offsets() {
        assert_eq!(parse_offset("UTC").unwrap().local_minus_utc(), 0);
        assert_eq!(parse_offset("+07:00").unwrap().local_minus_utc(), 7 * 3600);
        assert_eq!(parse_offset("-0530").unwrap().local_minus_utc(), -(5 * 3600 + 1800));
        assert_eq!(parse_offset("+7").unwrap().local_minus_utc(), 7 * 3600);
        assert!(parse_offset("Asia/Jakarta").is_err());
        assert!(parse_offset("+15:00").is_err());
    }
}
