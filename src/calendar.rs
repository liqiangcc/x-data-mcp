//! Explicit Shanghai calendar conversion shared by admission clocks.
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
pub fn shanghai_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
}
pub fn shanghai_today() -> NaiveDate {
    shanghai_date(Utc::now())
}
/// The Shanghai day of an RFC3339 timestamp, for facts recorded as instants.
///
/// The source store's `acquired_at` is a UTC instant (`…Z`); the day a row
/// became known is the day it was read *here*, so `2026-10-31T18:43:40Z` is
/// 2026-11-01 in Shanghai. Its UTC date would be one day early for every
/// snapshot stored between 16:00Z and 24:00Z, and a `known_at` filter would
/// then admit the row a day before we actually had it (XDM-157 review).
/// `index::observation_date` keeps its own UTC-day semantics: it names the
/// exchange's observation day, not our knowledge day.
pub fn shanghai_date_of(timestamp: &str) -> Option<NaiveDate> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(timestamp) {
        return Some(shanghai_date(instant.with_timezone(&Utc)));
    }
    // A bare `YYYY-MM-DD` is already a day, so it is taken as written.
    NaiveDate::parse_from_str(timestamp.get(..10)?, "%Y-%m-%d").ok()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_stored_instant_is_dated_in_shanghai_not_in_utc() {
        for (at, date) in [
            // The first `szse_short_name` snapshot: 02:43 Beijing time.
            ("2026-10-31T18:43:40Z", "2026-11-01"),
            ("2026-11-01T15:59:59Z", "2026-11-01"),
            ("2026-11-01T16:00:00Z", "2026-11-02"),
            ("2026-11-01", "2026-11-01"),
        ] {
            assert_eq!(
                shanghai_date_of(at),
                Some(date.parse::<NaiveDate>().unwrap()),
                "{at}"
            );
        }
        assert_eq!(shanghai_date_of("not a timestamp"), None);
    }
    #[test]
    fn shanghai_day_changes_at_utc_sixteen_including_year_boundary() {
        for (at, date) in [
            ("2026-09-12T15:59:59Z", "2026-09-12"),
            ("2026-09-12T16:00:00Z", "2026-09-13"),
            ("2026-12-31T16:00:00Z", "2027-01-01"),
        ] {
            assert_eq!(
                shanghai_date(at.parse().unwrap()),
                date.parse::<NaiveDate>().unwrap()
            );
        }
    }
}
