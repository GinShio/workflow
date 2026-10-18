//! The one clock, and the age specs a user writes against it.
//!
//! Two commands sweep stale things — `review prune` drops dormant MRs, `worktree
//! prune` reclaims dormant checkouts — and both let you say *how* stale with the
//! same `--older-than` spelling. That shared spelling is why this is a module
//! rather than a helper beside either caller.
//!
//! Time is **whole Unix seconds** everywhere, so a stored timestamp and a
//! computed cutoff are one type and compare directly with no conversion layer.
//! Absolute dates are handled without a date crate: an ISO day count is pure
//! arithmetic (see [`parse_cutoff`]), and nothing here needs a calendar beyond
//! that.

use anyhow::{bail, Context, Result};

/// The current Unix time in whole seconds.
///
/// A clock that cannot be read yields `0` rather than an error: every consumer
/// uses this to build a *cutoff*, and the epoch is the value that makes such a
/// cutoff select nothing instead of sweeping everything.
pub fn now_secs() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// How long ago `instant` was, in one coarse human phrase — `3 weeks ago`.
///
/// Coarse on purpose: this answers "is this stale?", where the unit carries the
/// whole message and a second component (`3 weeks, 2 days`) only adds noise. The
/// largest unit that yields a non-zero count wins.
///
/// A timestamp in the future reads as `just now` rather than a negative age: it
/// means a clock skewed somewhere, and inventing "in 3 days" would draw the eye
/// to a fact about the clock rather than about the worktree.
pub fn age_since(instant: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let elapsed = now_secs() - instant;
    if elapsed < MINUTE {
        return "just now".to_owned();
    }
    // Ordered coarsest-last; the first unit the elapsed time fills is the one.
    for (limit, unit, per) in [
        (HOUR, "minute", MINUTE),
        (DAY, "hour", HOUR),
        (WEEK, "day", DAY),
        (MONTH, "week", WEEK),
        (YEAR, "month", MONTH),
    ] {
        if elapsed < limit {
            return plural(elapsed / per, unit);
        }
    }
    plural(elapsed / YEAR, "year")
}

fn plural(count: i64, unit: &str) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{s} ago")
}

/// Interpret an `--older-than` spec and return the Unix instant before which a
/// thing counts as dormant. Accepts a relative age — `<N>`, `<N>d` (days),
/// `<N>w` (weeks) — or an absolute ISO-8601 date (`YYYY-MM-DD`).
///
/// A bare, unit-less number that is *shaped* like a year (four digits) is
/// rejected rather than read as "that many days ago": `--older-than 2026` is
/// almost always a mistyped date, and silently meaning ~5.5 years is worse than
/// a clear error asking for `2026d` or `2026-01-01`.
pub fn parse_cutoff(spec: &str) -> Result<i64> {
    let spec = spec.trim();

    // A dash means an absolute date; nothing else here contains one.
    if spec.contains('-') {
        let epoch_day = iso_date_to_epoch_day(spec)
            .with_context(|| format!("--older-than: '{spec}' is not a valid YYYY-MM-DD date"))?;
        return Ok(epoch_day * 86_400);
    }

    let (digits, per) = match spec.strip_suffix(['d', 'D']) {
        Some(n) => (n, 86_400),
        None => match spec.strip_suffix(['w', 'W']) {
            Some(n) => (n, 7 * 86_400),
            None => (spec, 86_400),
        },
    };
    let had_unit = digits.len() != spec.len();
    let n: i64 = digits.trim().parse().map_err(|_| {
        anyhow::anyhow!(
            "--older-than must be <days>, <N>d, <N>w, or a YYYY-MM-DD date, got '{spec}'"
        )
    })?;
    if !had_unit && (1000..=9999).contains(&n) {
        bail!(
            "--older-than '{spec}' is ambiguous (a bare four-digit number reads as a year, \
             not a day count); write '{n}d' for days or a full YYYY-MM-DD date"
        );
    }
    Ok(now_secs() - n.saturating_mul(per))
}

/// Days since the Unix epoch for a `YYYY-MM-DD` date (Hinnant's days_from_civil),
/// so an ISO date needs no date crate.
fn iso_date_to_epoch_day(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let y: i64 = parts.next()?.trim().parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// Days in a Gregorian month, so an impossible date like `2026-02-31` is
/// rejected rather than silently over-counting.
fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_disambiguates_days_units_and_dates() {
        let now = now_secs();
        // Bare count and explicit `d` agree, and are in the past.
        let bare = parse_cutoff("30").unwrap();
        let dayed = parse_cutoff("30d").unwrap();
        assert_eq!(bare, dayed);
        assert!(bare <= now && bare >= now - 31 * 86_400);
        // Weeks scale by 7.
        assert_eq!(parse_cutoff("2w").unwrap(), parse_cutoff("14d").unwrap());
        // A year-shaped bare number is refused; the same value with a unit is fine.
        assert!(parse_cutoff("2026").is_err());
        assert!(parse_cutoff("2026d").is_ok());
        // An ISO date is absolute.
        assert_eq!(parse_cutoff("1970-01-02").unwrap(), 86_400);
        assert!(parse_cutoff("2026-13-01").is_err());
    }

    #[test]
    fn age_picks_the_coarsest_unit_that_fills() {
        let ago = |secs: i64| age_since(now_secs() - secs);
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(3 * 60), "3 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(5 * 3600), "5 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(ago(7 * 86_400), "1 week ago");
        assert_eq!(ago(3 * 7 * 86_400), "3 weeks ago");
        assert_eq!(ago(30 * 86_400), "1 month ago");
        assert_eq!(ago(4 * 30 * 86_400), "4 months ago");
        assert_eq!(ago(365 * 86_400), "1 year ago");
        assert_eq!(ago(3 * 365 * 86_400), "3 years ago");
        // A future timestamp is a skewed clock, not a negative age.
        assert_eq!(age_since(now_secs() + 86_400), "just now");
    }

    #[test]
    fn iso_date_epoch_and_validation() {
        // The epoch itself, and a known post-epoch day.
        assert_eq!(iso_date_to_epoch_day("1970-01-01"), Some(0));
        assert_eq!(iso_date_to_epoch_day("1970-01-02"), Some(1));
        // Impossible dates are rejected rather than silently over-counted.
        assert!(iso_date_to_epoch_day("2026-02-31").is_none());
        assert!(iso_date_to_epoch_day("2026-13-01").is_none());
        assert!(iso_date_to_epoch_day("2026-00-10").is_none());
        // Leap-day handling.
        assert!(iso_date_to_epoch_day("2024-02-29").is_some());
        assert!(iso_date_to_epoch_day("2026-02-29").is_none());
    }
}
