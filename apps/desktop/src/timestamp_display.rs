//! Timestamp formatting for native conversation bubbles.
//!
//! Journal timestamps are Unix seconds, not elapsed durations. Render them
//! as an explicit UTC wall-clock minute without adding a timezone/dependency
//! or leaking the machine's local configuration into exported artifacts.

/// Converts Unix seconds (including fractional seconds) to a UTC minute.
/// Returns None for unrepresentable, non-finite, or unreasonable years.
pub(super) fn utc_minute(timestamp: f64) -> Option<String> {
    if !timestamp.is_finite() {
        return None;
    }
    let seconds = timestamp.floor();
    // Limit input before converting f64 to i64, and do not display medieval
    // dates caused by mistaking an event sequence for an epoch timestamp.
    if !(-11_676_096_000.0..253_402_300_800.0).contains(&seconds) {
        return None;
    }
    let seconds = seconds as i64;
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);

    // Proleptic Gregorian calendar: era-based civil-from-days conversion.
    // Unix day 0 is 1970-01-01. All intermediates fit i64 for the input range.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460
        + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    if !(1600..=9999).contains(&year) {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        seconds_of_day / 3_600,
        seconds_of_day % 3_600 / 60
    ))
}

#[cfg(test)]
mod tests {
    use super::utc_minute;

    #[test]
    fn utc_minutes_render_epoch_and_leap_years() {
        assert_eq!(utc_minute(0.0).as_deref(), Some("1970-01-01 00:00 UTC"));
        assert_eq!(utc_minute(86_400.0).as_deref(), Some("1970-01-02 00:00 UTC"));
        assert_eq!(
            utc_minute(951_782_400.0).as_deref(),
            Some("2000-02-29 00:00 UTC")
        );
        assert_eq!(
            utc_minute(1_791_756_090.0).as_deref(),
            Some("2026-10-11 22:01 UTC")
        );
        assert_eq!(
            utc_minute(1_600_000_000.75).as_deref(),
            Some("2020-09-13 12:26 UTC")
        );
    }

    #[test]
    fn malformed_epochs_do_not_become_misleading_metadata() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e99, -1e99, -12_000_000_000.0] {
            assert!(utc_minute(invalid).is_none());
        }
    }

    #[test]
    fn negative_epoch_floor_handles_day_boundary() {
        assert_eq!(utc_minute(-0.2).as_deref(), Some("1969-12-31 23:59 UTC"));
    }
}
