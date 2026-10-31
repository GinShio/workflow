//! Run ids and the calendar arithmetic behind them.
//!
//! An id is `YYYYMMDDThhmmssZ`, UTC, with a short `-N` suffix when two runs of
//! one case could start in the same second. The timestamp is the only part age
//! reads; the suffix never changes it.
//!
//! The arithmetic is Howard Hinnant's civil-date algorithm, which needs no
//! calendar crate: days since the Unix epoch and a civil date are exact
//! inverses for the range this tool will ever see.

/// The current Unix time in whole seconds.
pub fn now_secs() -> i64 {
    wits_core::time::now_secs()
}

/// The current run id.
pub fn now_run_id() -> String {
    run_id(now_secs())
}

/// A run id for a Unix instant.
pub fn run_id(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Whether `id` has the full run-id shape: the timestamp prefix and, when
/// present, a numeric collision suffix. This is the path-component check the
/// state file needs before an id is joined into the store.
pub fn is_run_id(id: &str) -> bool {
    let Some(timestamp) = parse_run_id(id) else {
        return false;
    };
    let prefix = run_id(timestamp);
    let Some(rest) = id.strip_prefix(&prefix) else {
        return false;
    };
    rest.is_empty()
        || rest
            .strip_prefix('-')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// The instant a run id's timestamp prefix names, or `None` when the id is not
/// a run id. The suffix is accepted and ignored.
pub fn parse_run_id(id: &str) -> Option<i64> {
    let bytes = id.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    if bytes[8] != b'T' || bytes[15] != b'Z' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = id.get(range)?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    let year = digits(0..4)?;
    let month = digits(4..6)?;
    let day = digits(6..8)?;
    let hour = digits(9..11)?;
    let minute = digits(11..13)?;
    let second = digits(13..15)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month as u32, day as u32);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// A human timestamp for the id's prefix, for `history`.
pub fn display_run_id(id: &str) -> String {
    let Some(secs) = parse_run_id(id) else {
        return id.to_owned();
    };
    let canonical = run_id(secs);
    format!(
        "{}-{}-{} {}:{}:{}Z",
        &canonical[0..4],
        &canonical[4..6],
        &canonical[6..8],
        &canonical[9..11],
        &canonical[11..13],
        &canonical[13..15]
    )
}

/// Append the collision suffix.
pub fn suffixed(id: &str, n: u32) -> String {
    format!("{id}-{n}")
}

/// The ordering key for ids: the timestamp, then the collision suffix. A
/// `Z-2` run is newer than the `Z` run that started in the same second, and a
/// `-10` run is newer than `-2`.
pub fn run_order(id: &str) -> (i64, u32) {
    let timestamp = parse_run_id(id).unwrap_or(0);
    let suffix = id
        .strip_prefix(&run_id(timestamp))
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    (timestamp, suffix)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month = i64::from(month);
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_the_epoch_and_known_dates() {
        assert_eq!(run_id(0), "19700101T000000Z");
        assert_eq!(parse_run_id("19700101T000000Z"), Some(0));
        for secs in [1, 86_399, 86_400, 1_600_000_000, 1_793_050_212] {
            let id = run_id(secs);
            assert_eq!(parse_run_id(&id), Some(secs), "{id}");
        }
        assert_eq!(run_id(1_793_050_212), "20261026T213012Z");
    }

    #[test]
    fn a_suffix_does_not_change_the_instant_or_the_display() {
        let id = suffixed("20261026T213012Z", 2);
        assert_eq!(parse_run_id(&id), parse_run_id("20261026T213012Z"));
        assert_eq!(display_run_id(&id), "2026-10-26 21:30:12Z");
    }

    #[test]
    fn a_non_id_is_rejected() {
        for id in [
            "",
            "2026",
            "20261301T000000Z",
            "20260101T240000Z",
            "not-a-run",
        ] {
            assert_eq!(parse_run_id(id), None, "{id}");
        }
    }
}
