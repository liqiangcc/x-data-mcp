//! Explicit Shanghai calendar conversion shared by admission clocks.
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
pub fn shanghai_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap())
        .date_naive()
}
pub fn shanghai_today() -> NaiveDate {
    shanghai_date(Utc::now())
}
#[cfg(test)]
mod tests {
    use super::*;
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
