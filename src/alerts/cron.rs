//! A five-field cron expression (minute hour day-of-month month day-of-week), UTC. Supports `*`,
//! lists, ranges and `/step`; day-of-week 0 or 7 is Sunday. When both day fields are restricted a
//! time matches if either does, as in classic cron.

use chrono::{Datelike, TimeZone, Timelike, Utc};

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: u64,
    hour: u64,
    dom: u64,
    month: u64,
    dow: u64,
    dom_any: bool,
    dow_any: bool,
}

fn field(s: &str, lo: u32, hi: u32, name: &str) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let any = s == "*";
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (
                r,
                st.parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(format!("bad step in {name}"))?,
            ),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (
                a.parse::<u32>().map_err(|_| format!("bad {name}"))?,
                b.parse::<u32>().map_err(|_| format!("bad {name}"))?,
            )
        } else {
            let v = range.parse::<u32>().map_err(|_| format!("bad {name}"))?;
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{name} must be within {lo}-{hi}"));
        }
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let f: Vec<&str> = expr.split_whitespace().collect();
        if f.len() != 5 || expr.len() > 100 {
            return Err(
                "cron needs five fields: minute hour day-of-month month day-of-week".into(),
            );
        }
        let (minute, _) = field(f[0], 0, 59, "minute")?;
        let (hour, _) = field(f[1], 0, 23, "hour")?;
        let (dom, dom_any) = field(f[2], 1, 31, "day of month")?;
        let (month, _) = field(f[3], 1, 12, "month")?;
        let (mut dow, dow_any) = field(f[4], 0, 7, "day of week")?;
        if dow & (1 << 7) != 0 {
            dow |= 1;
        }
        Ok(Cron {
            minute,
            hour,
            dom,
            month,
            dow,
            dom_any,
            dow_any,
        })
    }

    fn day_ok(&self, d: chrono::DateTime<Utc>) -> bool {
        let dom = self.dom & (1 << d.day()) != 0;
        let dow = self.dow & (1 << d.weekday().num_days_from_sunday()) != 0;
        match (self.dom_any, self.dow_any) {
            (false, false) => dom || dow,
            (false, true) => dom,
            (true, false) => dow,
            (true, true) => true,
        }
    }

    pub fn matches(&self, ms: i64) -> bool {
        let Some(d) = Utc.timestamp_millis_opt(ms).single() else {
            return false;
        };
        self.minute & (1 << d.minute()) != 0
            && self.hour & (1 << d.hour()) != 0
            && self.month & (1 << d.month()) != 0
            && self.day_ok(d)
    }

    /// The first matching minute strictly after `ms` (within four years), as epoch ms.
    pub fn next_after(&self, ms: i64) -> Option<i64> {
        let mut t = (ms.div_euclid(60_000) + 1) * 60_000;
        for _ in 0..(366 * 4 * 24 * 60) {
            if self.matches(t) {
                return Some(t);
            }
            t += 60_000;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_steps() {
        let c = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        // 2026-01-05 is a Monday.
        let t = Utc
            .with_ymd_and_hms(2026, 1, 5, 9, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert!(c.matches(t));
        assert_eq!(c.next_after(t), Some(t + 15 * 60_000));
        assert!(Cron::parse("61 * * * *").is_err());
        assert!(Cron::parse("* * *").is_err());
    }
}
