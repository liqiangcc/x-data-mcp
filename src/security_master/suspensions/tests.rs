use chrono::{Datelike, NaiveDate};

use super::*;
use crate::security_master::{
    DelistingReason, DelistingSource, IssuerResolution, ListingSource, SecurityLifecycleFact,
};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// Weekdays in the closed interval. The production calendar is derived data
/// (M1) and this module never reads it from storage, so a weekday list is the
/// honest stand-in: nothing here depends on which weekdays were actually
/// trading days in 2006.
fn weekdays(from: NaiveDate, to: NaiveDate) -> Vec<NaiveDate> {
    let mut days = Vec::new();
    let mut cursor = from;
    while cursor <= to {
        if cursor.weekday().num_days_from_monday() < 5 {
            days.push(cursor);
        }
        cursor = cursor.succ_opt().unwrap();
    }
    days
}

fn fact(
    exchange: Exchange,
    code: &str,
    from: NaiveDate,
    to: Option<NaiveDate>,
    listing: Option<NaiveDate>,
    delisting: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: listing,
        listing_date_tdx: listing,
        listing_source: if listing.is_some() {
            ListingSource::ExchangeList
        } else {
            ListingSource::Unknown
        },
        b_listing_date: None,
        delisting_date: delisting,
        delisting_date_tdx: delisting,
        delisting_source: if delisting.is_some() {
            DelistingSource::ExchangeList
        } else {
            DelistingSource::Unknown
        },
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: None,
    }
}

fn confirmed(
    exchange: Exchange,
    code: &str,
    start: NaiveDate,
    end: NaiveDate,
    known_at: Option<NaiveDate>,
) -> ConfirmedSuspensionFact {
    ConfirmedSuspensionFact {
        exchange,
        code: code.into(),
        start_day: start,
        end_day: end,
        reason: SuspensionReason::ExchangeAnnouncement,
        known_at,
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("fixture-1".into()),
    }
}

fn admitted(facts: Vec<ConfirmedSuspensionFact>) -> SuspensionSource {
    suspension_source(Some(ConfirmedSuspensionSnapshot {
        as_of: day(2026, 10, 26),
        facts,
    }))
}

/// AC-M2-8's fixture: `sz000631` has a bar on 2006-04-27 (0.76) and the next
/// one on 2009-06-05 (10.30), and nothing in between. Without an admitted
/// announcement the whole stretch is one derived `unresolved_gap` — never a
/// suspension — which is what makes `tradability` answer `unknown`/`data_gap`
/// (§7.4 step 4).
#[test]
fn sz000631_derives_one_unresolved_gap_and_no_confirmed_row() {
    let calendar = weekdays(day(2006, 4, 27), day(2009, 6, 5));
    let facts = vec![fact(
        Exchange::Sz,
        "000631",
        day(1996, 1, 2),
        None,
        Some(day(1996, 1, 2)),
        None,
    )];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sz.000631".to_string(),
        vec![day(2006, 4, 27), day(2009, 6, 5)],
    );

    let source = suspension_source(None);
    assert!(matches!(source, SuspensionSource::Pending(_)));
    let set = assemble_suspensions(&source, &facts, &calendar, &bars).unwrap();

    assert_eq!(
        set.confirmed_rows, 0,
        "a pending source writes zero confirmed rows"
    );
    assert_eq!(set.pending_sources.len(), 1);
    assert_eq!(set.pending_sources[0].dataset, DATASET_KIND);
    assert_eq!(set.gap_rows, 1);
    assert_eq!(set.single_day_gap_rows, 0);
    assert_eq!(set.multi_day_gap_rows, 1);

    let row = &set.rows[0];
    assert_eq!(row.entity(), "sz.000631");
    assert_eq!(row.start_day, day(2006, 4, 28));
    assert_eq!(row.end_day, day(2009, 6, 4));
    // 2006-04-27 and 2009-06-05 are the bracketing bars; every weekday after
    // the first and before the second is a missing trading day.
    let expected = weekdays(day(2006, 4, 28), day(2009, 6, 4)).len() as i64;
    assert_eq!(row.missing_days, expected);
    assert_eq!(set.missing_days(), expected);
    assert_eq!(row.certainty, SuspensionCertainty::UnresolvedGap);
    assert_eq!(row.reason, SuspensionReason::MissingBarNoEvent);
    assert_eq!(row.known_at, None, "a derived gap has no announcement day");
    assert_eq!(row.source_kind, GAP_SOURCE_KIND);
}

/// §7.3: nothing is derived before the first bar (listing/start) or at/after
/// the delisting day, so a listing gap and a post-delisting silence never turn
/// into `unresolved_gap` rows.
#[test]
fn no_gap_before_listing_or_after_delisting() {
    let calendar = weekdays(day(2024, 1, 2), day(2024, 1, 31));
    let facts = vec![fact(
        Exchange::Sz,
        "000001",
        day(2024, 1, 1),
        None,
        Some(day(2024, 1, 10)),
        Some(day(2024, 1, 20)),
    )];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sz.000001".to_string(),
        vec![day(2024, 1, 10), day(2024, 1, 19)],
    );

    let set = assemble_suspensions(&suspension_source(None), &facts, &calendar, &bars).unwrap();
    assert_eq!(set.gap_rows, 1);
    let row = &set.rows[0];
    // 01-02..01-09 are before the listing date (and before the first bar).
    assert_eq!(row.start_day, day(2024, 1, 11));
    assert_eq!(row.end_day, day(2024, 1, 18));
    assert_eq!(row.missing_days, 6);
    // 01-20 is the delisting day: the interval ends there, and the trading days
    // after the last bar (01-22 onwards) are not a gap.
    assert!(row.end_day < day(2024, 1, 20));
}

/// §7.3: `missing_days = 1` and longer runs are counted apart so a one-off data
/// hole is distinguishable from a long outage.
#[test]
fn single_day_gaps_and_long_gaps_are_counted_apart() {
    let calendar = weekdays(day(2024, 3, 4), day(2024, 3, 15));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 3, 4),
        None,
        Some(day(2024, 3, 4)),
        None,
    )];
    // Weekdays are 03-04…03-08 and 03-11…03-15. Bars on 03-04, 03-05, 03-06,
    // 03-08 and 03-15: one single-day hole (03-07) and one long run
    // (03-11..03-14).
    let mut bars = BTreeMap::new();
    bars.insert(
        "sh.600000".to_string(),
        vec![
            day(2024, 3, 4),
            day(2024, 3, 5),
            day(2024, 3, 6),
            day(2024, 3, 8),
            day(2024, 3, 15),
        ],
    );

    let set = assemble_suspensions(&suspension_source(None), &facts, &calendar, &bars).unwrap();
    assert_eq!(set.gap_rows, 2);
    assert_eq!(set.single_day_gap_rows, 1);
    assert_eq!(set.multi_day_gap_rows, 1);
    assert_eq!(set.missing_days(), 5);
    assert_eq!(
        (set.rows[0].start_day, set.rows[0].missing_days),
        (day(2024, 3, 7), 1)
    );
    assert_eq!(
        (set.rows[1].start_day, set.rows[1].end_day),
        (day(2024, 3, 11), day(2024, 3, 14))
    );
    assert_eq!(set.rows[1].missing_days, 4);
}

/// A confirmed interval is an *explanation*: it splits the gap run, keeps its
/// own row (with the announcement day as `known_at`) and counts only the
/// trading days it covers.
#[test]
fn confirmed_rows_explain_and_split_the_gap_run() {
    let calendar = weekdays(day(2024, 5, 6), day(2024, 5, 17));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 5, 6),
        None,
        Some(day(2024, 5, 6)),
        None,
    )];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sh.600000".to_string(),
        vec![day(2024, 5, 6), day(2024, 5, 17)],
    );
    let source = admitted(vec![confirmed(
        Exchange::Sh,
        "600000",
        day(2024, 5, 8),
        day(2024, 5, 9),
        Some(day(2024, 5, 7)),
    )]);

    let set = assemble_suspensions(&source, &facts, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 1);
    assert_eq!(set.gap_rows, 2);
    assert!(set.pending_sources.is_empty());

    // Row identity order: the confirmed row starts before the second gap.
    let confirmed_row = set
        .rows
        .iter()
        .find(|row| row.certainty == SuspensionCertainty::Confirmed)
        .expect("the admitted fact is stored");
    assert_eq!(confirmed_row.start_day, day(2024, 5, 8));
    assert_eq!(confirmed_row.end_day, day(2024, 5, 9));
    assert_eq!(confirmed_row.missing_days, 2);
    assert_eq!(confirmed_row.reason, SuspensionReason::ExchangeAnnouncement);
    assert_eq!(confirmed_row.known_at, Some(day(2024, 5, 7)));
    assert_eq!(confirmed_row.source_record_id.as_deref(), Some("fixture-1"));

    let gaps: Vec<&SuspensionInterval> = set
        .rows
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
        .collect();
    assert_eq!(gaps.len(), 2);
    assert_eq!(
        (gaps[0].start_day, gaps[0].end_day, gaps[0].missing_days),
        (day(2024, 5, 7), day(2024, 5, 7), 1)
    );
    assert_eq!(
        (gaps[1].start_day, gaps[1].end_day, gaps[1].missing_days),
        (day(2024, 5, 10), day(2024, 5, 16), 5)
    );
}

/// The gap derivation is scoped to A shares (§16 PR-6, M2 scope). A future
/// instrument class cannot inherit intraday-bar gaps by accident.
#[test]
fn non_a_share_intervals_produce_no_gap() {
    let calendar = weekdays(day(2024, 6, 3), day(2024, 6, 7));
    let mut row = fact(
        Exchange::Sh,
        "000300",
        day(2024, 6, 3),
        None,
        Some(day(2024, 6, 3)),
        None,
    );
    row.instrument_class = 1;
    let facts = vec![row];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sh.000300".to_string(),
        vec![day(2024, 6, 3), day(2024, 6, 7)],
    );
    let set = assemble_suspensions(&suspension_source(None), &facts, &calendar, &bars).unwrap();
    assert!(set.is_empty());
}

#[test]
fn confirmed_validation_rejects_bad_ranges_reasons_and_overlaps() {
    let start = day(2024, 1, 10);
    let end = day(2024, 1, 12);

    let reversed = confirmed(Exchange::Sh, "600000", end, start, None);
    assert_eq!(
        validate_confirmed(std::slice::from_ref(&reversed)),
        Err(Error::Range),
        "an interval that ends before it starts must not be stored"
    );

    let mut derived_reason = confirmed(Exchange::Sh, "600000", start, end, None);
    derived_reason.reason = SuspensionReason::MissingBarNoEvent;
    assert_eq!(
        validate_confirmed(std::slice::from_ref(&derived_reason)),
        Err(Error::Reason)
    );

    let mut empty_kind = confirmed(Exchange::Sh, "600000", start, end, None);
    empty_kind.source_kind = "  ".into();
    assert_eq!(
        validate_confirmed(std::slice::from_ref(&empty_kind)),
        Err(Error::SourceKind)
    );

    // Same code, overlapping intervals: ambiguous instead of fail-closed.
    assert_eq!(
        validate_confirmed(&[
            confirmed(Exchange::Sh, "600000", start, end, None),
            confirmed(Exchange::Sh, "600000", end, day(2024, 1, 15), None),
        ]),
        Err(Error::Overlap)
    );
    // Different codes may overlap.
    assert!(
        validate_confirmed(&[
            confirmed(Exchange::Sh, "600000", start, end, None),
            confirmed(Exchange::Sz, "000001", start, end, None),
        ])
        .is_ok()
    );
}

#[test]
fn row_validation_enforces_the_certainty_reason_and_known_at_invariants() {
    let gap = SuspensionInterval {
        exchange: Exchange::Sz,
        code: "000631".into(),
        start_day: day(2024, 1, 10),
        end_day: day(2024, 1, 12),
        certainty: SuspensionCertainty::UnresolvedGap,
        missing_days: 3,
        reason: SuspensionReason::MissingBarNoEvent,
        known_at: None,
        source_kind: GAP_SOURCE_KIND.into(),
        source_record_id: None,
    };
    assert_eq!(validate_set(std::slice::from_ref(&gap)), Ok(()));

    let mut with_known_at = gap.clone();
    with_known_at.known_at = Some(day(2024, 1, 9));
    assert_eq!(
        validate_set(std::slice::from_ref(&with_known_at)),
        Err(Error::DerivedKnownAt)
    );

    let mut wrong_reason = gap.clone();
    wrong_reason.reason = SuspensionReason::ExchangeAnnouncement;
    assert_eq!(
        validate_set(std::slice::from_ref(&wrong_reason)),
        Err(Error::GapReason)
    );

    let mut zero_days = gap.clone();
    zero_days.missing_days = 0;
    assert_eq!(
        validate_set(std::slice::from_ref(&zero_days)),
        Err(Error::MissingDays)
    );

    assert_eq!(validate_set(&[gap.clone(), gap]), Err(Error::Duplicate));
}

#[test]
fn a_confirmed_fact_for_an_unknown_entity_is_refused() {
    let calendar = weekdays(day(2024, 1, 8), day(2024, 1, 12));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 1, 8),
        None,
        Some(day(2024, 1, 8)),
        None,
    )];
    let bars = BTreeMap::new();

    // The adapter emitted a code the main table does not carry.
    let unknown_entity = admitted(vec![confirmed(
        Exchange::Sz,
        "000001",
        day(2024, 1, 10),
        day(2024, 1, 11),
        None,
    )]);
    assert_eq!(
        assemble_suspensions(&unknown_entity, &facts, &calendar, &bars),
        Err(Error::UnknownEntity)
    );
}

/// XDM-134: a weekend-only interval has no trading day to explain, so it writes
/// no row (the DDL forbids `missing_days = 0`) and is counted and sampled
/// instead — one weekend cannot fail the whole market's suspension dataset.
#[test]
fn a_weekend_only_interval_is_skipped_with_a_sample() {
    let calendar = weekdays(day(2024, 1, 8), day(2024, 1, 19));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 1, 8),
        None,
        Some(day(2024, 1, 8)),
        None,
    )];
    let bars = BTreeMap::new();
    let weekend = admitted(vec![confirmed(
        Exchange::Sh,
        "600000",
        day(2024, 1, 13),
        day(2024, 1, 14),
        None,
    )]);

    let set = assemble_suspensions(&weekend, &facts, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 0);
    assert!(
        set.rows.is_empty(),
        "no row can carry a positive missing_days"
    );
    assert_eq!(set.skipped_no_trading_day, 1);
    assert_eq!(set.deferred_beyond_calendar, 0);
    assert_eq!(set.skipped_samples.len(), 1);
    let sample = &set.skipped_samples[0];
    assert_eq!(sample.entity, "sh.600000");
    assert_eq!(
        (sample.start_day, sample.end_day),
        (day(2024, 1, 13), day(2024, 1, 14))
    );
    assert_eq!(sample.source_record_id.as_deref(), Some("fixture-1"));
}

/// XDM-134: an interval the calendar does not fully cover defers whole. Writing
/// it with only the covered trading days would understate `missing_days`, so a
/// partially covered interval is deferred too, never clamped.
#[test]
fn an_interval_past_the_calendar_last_day_is_deferred_whole() {
    let calendar = weekdays(day(2026, 10, 1), day(2026, 10, 28));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2026, 9, 1),
        None,
        Some(day(2026, 9, 1)),
        None,
    )];
    let bars = BTreeMap::new();

    // The whole interval is after `last_day` (10-28): a suspension starting on
    // 10-29 cannot be placed yet.
    let after = admitted(vec![confirmed(
        Exchange::Sh,
        "600000",
        day(2026, 10, 29),
        day(2026, 10, 29),
        None,
    )]);
    let set = assemble_suspensions(&after, &facts, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 0);
    assert!(set.rows.is_empty());
    assert_eq!(set.deferred_beyond_calendar, 1);
    assert_eq!(set.skipped_no_trading_day, 0);
    assert_eq!(set.deferred_samples.len(), 1);
    assert_eq!(set.deferred_samples[0].entity, "sh.600000");
    assert_eq!(set.deferred_samples[0].start_day, day(2026, 10, 29));

    // A partially covered interval (10-20..10-29, calendar to 10-28) is
    // deferred as a whole: no partial `missing_days` row is written. The
    // covered part alone holds seven trading days, so before this rule the row
    // would have been written with `missing_days = 7` and understated.
    assert_eq!(
        trading_days_within(&calendar, day(2026, 10, 20), day(2026, 10, 28)),
        7
    );
    let partial = admitted(vec![confirmed(
        Exchange::Sh,
        "600000",
        day(2026, 10, 20),
        day(2026, 10, 29),
        None,
    )]);
    let set = assemble_suspensions(&partial, &facts, &calendar, &bars).unwrap();
    assert!(
        set.rows.is_empty(),
        "a partial count is understated, not written"
    );
    assert_eq!(set.deferred_beyond_calendar, 1);
    assert_eq!(set.skipped_no_trading_day, 0);
}

/// A fully covered interval keeps the existing rule: `missing_days` is the
/// number of calendar trading days in it, and the skipped shapes on other
/// entities do not disturb that row.
#[test]
fn a_covered_interval_keeps_its_trading_day_count() {
    let calendar = weekdays(day(2024, 1, 8), day(2024, 1, 19));
    let facts = vec![
        fact(
            Exchange::Sh,
            "600000",
            day(2024, 1, 8),
            None,
            Some(day(2024, 1, 8)),
            None,
        ),
        fact(
            Exchange::Sz,
            "000001",
            day(2024, 1, 8),
            None,
            Some(day(2024, 1, 8)),
            None,
        ),
    ];
    let bars = BTreeMap::new();
    let source = admitted(vec![
        // Friday..Monday covers two trading days (01-12 and 01-15).
        confirmed(
            Exchange::Sh,
            "600000",
            day(2024, 1, 12),
            day(2024, 1, 15),
            None,
        ),
        // A weekend block on another entity is skipped, not written.
        confirmed(
            Exchange::Sz,
            "000001",
            day(2024, 1, 13),
            day(2024, 1, 14),
            None,
        ),
    ]);

    let set = assemble_suspensions(&source, &facts, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 1);
    assert_eq!(set.skipped_no_trading_day, 1);
    assert_eq!(set.deferred_beyond_calendar, 0);
    let row = &set.rows[0];
    assert_eq!(row.entity(), "sh.600000");
    assert_eq!(row.missing_days, 2);
    assert_eq!(
        (row.start_day, row.end_day),
        (day(2024, 1, 12), day(2024, 1, 15))
    );
}

/// A skipped interval is not `explained`: it does not split a gap run and adds
/// no row, because the days it covers were never trading days.
#[test]
fn a_skipped_interval_neither_splits_a_gap_nor_adds_a_row() {
    let calendar = weekdays(day(2024, 1, 8), day(2024, 1, 19));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 1, 8),
        None,
        Some(day(2024, 1, 8)),
        None,
    )];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sh.600000".to_string(),
        vec![day(2024, 1, 8), day(2024, 1, 19)],
    );
    let source = admitted(vec![confirmed(
        Exchange::Sh,
        "600000",
        day(2024, 1, 13),
        day(2024, 1, 14),
        None,
    )]);

    let set = assemble_suspensions(&source, &facts, &calendar, &bars).unwrap();
    assert_eq!(set.confirmed_rows, 0);
    assert_eq!(set.skipped_no_trading_day, 1);
    assert_eq!(set.gap_rows, 1, "the weekend block splits nothing");
    let gap = &set.rows[0];
    assert_eq!(
        (gap.start_day, gap.end_day),
        (day(2024, 1, 9), day(2024, 1, 18))
    );
    assert_eq!(gap.missing_days, 8);
}

/// The dataset identity is the stored rows only: a skipped fact leaves the
/// digest equal to the same derivation without it.
#[test]
fn a_skipped_interval_does_not_enter_the_digest() {
    let calendar = weekdays(day(2024, 1, 8), day(2024, 1, 19));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 1, 8),
        None,
        Some(day(2024, 1, 8)),
        None,
    )];
    let bars = BTreeMap::new();
    let with_weekend = assemble_suspensions(
        &admitted(vec![confirmed(
            Exchange::Sh,
            "600000",
            day(2024, 1, 13),
            day(2024, 1, 14),
            None,
        )]),
        &facts,
        &calendar,
        &bars,
    )
    .unwrap();
    let without = assemble_suspensions(&admitted(Vec::new()), &facts, &calendar, &bars).unwrap();

    assert!(with_weekend.rows.is_empty());
    assert_eq!(
        dataset_digest(&with_weekend.rows),
        dataset_digest(&without.rows)
    );
}

/// The counters stay complete when the sample list is truncated at
/// [`SKIP_SAMPLE_LIMIT`]: a truncated list must not look like fewer skips.
#[test]
fn skip_counts_stay_complete_when_samples_are_truncated() {
    let calendar = weekdays(day(2024, 1, 1), day(2024, 6, 30));
    let facts = vec![fact(
        Exchange::Sh,
        "600000",
        day(2024, 1, 1),
        None,
        Some(day(2024, 1, 1)),
        None,
    )];
    let bars = BTreeMap::new();
    let mut weekend_facts = Vec::new();
    for week in 0..(SKIP_SAMPLE_LIMIT + 2) {
        let saturday = day(2024, 1, 6) + chrono::Duration::weeks(i64::try_from(week).unwrap());
        weekend_facts.push(confirmed(
            Exchange::Sh,
            "600000",
            saturday,
            saturday.succ_opt().unwrap(),
            None,
        ));
    }

    let set = assemble_suspensions(&admitted(weekend_facts), &facts, &calendar, &bars).unwrap();
    assert_eq!(set.skipped_no_trading_day, SKIP_SAMPLE_LIMIT + 2);
    assert_eq!(set.skipped_samples.len(), SKIP_SAMPLE_LIMIT);
    assert!(set.rows.is_empty());
}

/// §3.10: the digest covers the rows, not the order they were derived in, and
/// it changes when a row changes.
#[test]
fn the_dataset_digest_is_order_independent_and_content_sensitive() {
    let calendar = weekdays(day(2024, 7, 1), day(2024, 7, 12));
    let facts = vec![
        fact(
            Exchange::Sh,
            "600000",
            day(2024, 7, 1),
            None,
            Some(day(2024, 7, 1)),
            None,
        ),
        fact(
            Exchange::Sz,
            "000001",
            day(2024, 7, 1),
            None,
            Some(day(2024, 7, 1)),
            None,
        ),
    ];
    let mut bars = BTreeMap::new();
    bars.insert(
        "sh.600000".to_string(),
        vec![day(2024, 7, 1), day(2024, 7, 12)],
    );
    bars.insert(
        "sz.000001".to_string(),
        vec![day(2024, 7, 1), day(2024, 7, 12)],
    );

    let first = assemble_suspensions(&suspension_source(None), &facts, &calendar, &bars).unwrap();
    let mut reversed = facts.clone();
    reversed.reverse();
    let second =
        assemble_suspensions(&suspension_source(None), &reversed, &calendar, &bars).unwrap();
    assert_eq!(first.rows, second.rows);
    assert_eq!(
        dataset_digest(&first.rows),
        dataset_digest(&second.rows),
        "row identity order makes the digest independent of derivation order"
    );

    let mut shorter = first.rows.clone();
    shorter[0].missing_days -= 1;
    shorter[0].end_day = shorter[0].end_day.pred_opt().unwrap();
    assert_ne!(dataset_digest(&first.rows), dataset_digest(&shorter));
}
