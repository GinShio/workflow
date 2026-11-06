//! Run ids and the calendar arithmetic behind them.
//!
//! An id is `YYYYMMDDThhmmssZ`: the UTC second the run started. The width is
//! fixed, so ordering ids as text orders them in time.
//!
//! The arithmetic is Howard Hinnant's civil-date algorithm, which needs no
//! calendar crate: days since the Unix epoch and a civil date are exact
//! inverses for the range this tool will ever see.

/// The current run id.
pub fn now_run_id() -> String {
    run_id(wits_core::time::now_secs())
}

/// The run id for a Unix instant.
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

/// The instant a run id names, or `None` when `id` is not exactly one.
pub fn parse_run_id(id: &str) -> Option<i64> {
    let bytes = id.as_bytes();
    if bytes.len() != 16 || bytes[8] != b'T' || bytes[15] != b'Z' {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let text = id.get(range)?;
        if !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok()
    };
    let days = days_from_civil(number(0..4)?, number(4..6)?, number(6..8)?);
    let secs = days * 86_400 + number(9..11)? * 3600 + number(11..13)? * 60 + number(13..15)?;
    // An out-of-range field (month 13, hour 24, 30 February) lands on another
    // instant, whose id is spelled differently.
    (run_id(secs) == id).then_some(secs)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_the_epoch_and_known_dates() {
        assert_eq!(run_id(0), "19700101T000000Z");
        for secs in [
            0,
            1,
            86_399,
            86_400,
            951_782_400,
            1_600_000_000,
            1_793_050_212,
        ] {
            let id = run_id(secs);
            assert_eq!(parse_run_id(&id), Some(secs), "{id}");
        }
        assert_eq!(run_id(1_793_050_212), "20261026T213012Z");
        // 2000-02-29 exists; text order is time order.
        assert_eq!(run_id(951_782_400), "20000229T000000Z");
        assert!(run_id(1_600_000_000) < run_id(1_600_000_001));
    }

    #[test]
    fn a_non_id_is_rejected() {
        for id in [
            "",
            "2026",
            "20261301T000000Z",
            "20260101T240000Z",
            "20260230T000000Z",
            "20261026T213012Z-2",
            "2026102xT213012Z",
            "not-a-run-id-123",
        ] {
            assert_eq!(parse_run_id(id), None, "{id}");
        }
    }
}
