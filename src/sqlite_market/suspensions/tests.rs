//! `suspension_intervals` writer tests (issue-391 §16 PR-6; AC-M2-8).
//!
//! The fixture writes through the production paths only: the main table
//! (`security::replace_security_master`), a 不复权 daily series
//! (`series::insert_series`/`insert_bars`), the calendar
//! (`calendar::commit_calendar`), then the derivation
//! (`suspensions::assemble_suspensions`) and this module's writer. The read
//! model then answers through `research::tradability`, so the test connects
//! the derived row to the AC's verdict instead of asserting on a hand-built
//! row.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::{Datelike, NaiveDate};
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::compact_market::{CompactBar, Digest, EvidenceKind, SourceBinding, VersionEvidence};
use crate::market_data::{
    Adjustment, AdjustmentEvidence, CalendarBatch, CalendarInputDates, CoverageStatus,
    EvidenceValue, INSTRUMENT_CLASS_STOCK, Period, UnitCurrencyEvidence,
};
use crate::research::temporal::Temporal;
use crate::research::tradability::{Tradability, TradabilityReason, tradability};
use crate::security_master::assembly::Assembly;
use crate::security_master::suspensions::{
    ConfirmedSuspensionFact, ConfirmedSuspensionSnapshot, SuspensionSet, assemble_suspensions,
    suspension_source,
};
use crate::security_master::{
    DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact, SuspensionCertainty, SuspensionReason,
};
use crate::sqlite_market::calendar::{self, CalendarWrite};
use crate::sqlite_market::evidence_store;
use crate::sqlite_market::series::NewSeries;
use crate::sqlite_market::{security, series};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_calendar::TradingCalendar;

const GEN: &str = "gen-1";
const MANIFEST_SHA: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const CREATED_AT: &str = "2026-10-27T00:00:00Z";

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

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

fn bar(day: NaiveDate, close_units: i64) -> CompactBar {
    CompactBar {
        trade_day: crate::compact_market::epoch_day(day),
        open_units: close_units - 10,
        high_units: close_units + 10,
        low_units: close_units - 20,
        close_units,
        volume: Some(100),
        amount: Some("100500.00".into()),
        r7_raw: Some(7),
    }
}

fn digest(seed: u8) -> Digest {
    [seed; 32]
}

fn series_evidence() -> serde_json::Value {
    serde_json::to_value(VersionEvidence {
        source: "official".into(),
        coverage_reason: "fixture".into(),
        unit_currency: UnitCurrencyEvidence {
            price_currency: EvidenceValue::confirmed("CNY", "bound"),
            price_unit: EvidenceValue::confirmed("share", "bound"),
            volume_unit: EvidenceValue::confirmed("share", "bound"),
            amount_currency: EvidenceValue::confirmed("CNY", "bound"),
            amount_unit: EvidenceValue::confirmed("yuan", "bound"),
        },
        adjustment_evidence: AdjustmentEvidence::confirmed("raw", "bound"),
        excluded_bars: None,
    })
    .unwrap()
}

fn lifecycle(
    code: &str,
    from: NaiveDate,
    listing: Option<NaiveDate>,
    delisting: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange: Exchange::Sz,
        code: code.into(),
        effective_from: from,
        effective_to: None,
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
        // The main table's own 知晓时间, so a `known_at` filter that targets a
        // suspension does not also drop the membership row.
        known_at: Some(from),
    }
}

/// The AC-M2-8 fixture: `sz000631` has a bar on 2006-04-27 (0.76) and the next
/// on 2009-06-05 (10.30), nothing in between.
struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    c: Connection,
    binding: Digest,
    calendar: Vec<NaiveDate>,
    bars: BTreeMap<String, Vec<NaiveDate>>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let path = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();

        let generation = evidence_store::put_evidence(
            &c,
            EvidenceKind::Generation,
            &json!({"generation_id": GEN, "manifest_sha256": MANIFEST_SHA}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &c,
            &SourceBinding {
                generation_id: GEN.into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        let version_evidence =
            evidence_store::put_evidence(&c, EvidenceKind::Version, &series_evidence()).unwrap();

        let bar_days = vec![day(2006, 4, 27), day(2009, 6, 5)];
        let bars: Vec<CompactBar> = bar_days
            .iter()
            .map(|day| bar(*day, 1_000 + crate::compact_market::epoch_day(*day) % 100))
            .collect();
        let key = series::insert_series(
            &c,
            &NewSeries {
                entity: "sz.000631".into(),
                period: Period::Day,
                adjustment: Adjustment::Raw,
                instrument_class: INSTRUMENT_CLASS_STOCK,
                price_scale: 2,
                content_digest: digest(0x31),
                record_count: bars.len() as i64,
                first_day: bars[0].trade_day,
                last_day: bars[bars.len() - 1].trade_day,
                coverage_status: CoverageStatus::Complete,
                source_binding_digest: binding,
                evidence_digest: version_evidence,
                last_event_seq: 1,
                created_at: CREATED_AT.into(),
                updated_at: CREATED_AT.into(),
            },
        )
        .unwrap();
        series::insert_bars(&c, key, &bars).unwrap();
        let mut bar_map = BTreeMap::new();
        bar_map.insert("sz.000631".to_string(), bar_days);

        security::replace_security_master(
            &c,
            &Assembly {
                rows: vec![lifecycle(
                    "000631",
                    day(1996, 1, 2),
                    Some(day(1996, 1, 2)),
                    None,
                )],
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: None,
            },
            binding,
            "task-master",
            "derived:security_master",
        )
        .unwrap();

        let calendar_days = weekdays(day(2006, 4, 27), day(2009, 6, 5));
        // The union requires every registered input (§5.1), so the fixture
        // supplies both index series with the same date list.
        let batch = CalendarBatch {
            inputs: vec![
                CalendarInputDates {
                    entity: "sh.000001".into(),
                    member: "sh\\lday\\sh000001.day".into(),
                    day_member_sha256: "aa".repeat(32),
                    days: calendar_days.clone(),
                },
                CalendarInputDates {
                    entity: "sz.399001".into(),
                    member: "sz\\lday\\sz399001.day".into(),
                    day_member_sha256: "bb".repeat(32),
                    days: calendar_days.clone(),
                },
            ],
            security_event_sha256: crate::hash::sha256(&0u64.to_le_bytes()),
        };
        let calendar = TradingCalendar::union(&batch).unwrap();
        let coverage = calendar::measure_coverage(&c, &calendar).unwrap();
        calendar::commit_calendar(
            &c,
            &CalendarWrite {
                batch: &batch,
                coverage: &coverage,
                task_id: "task-calendar",
                item_id: "derived:calendar",
                generation_id: GEN,
                generation_manifest_sha256: MANIFEST_SHA,
                generation_evidence: &json!({"generation_id": GEN}),
            },
        )
        .unwrap();

        Self {
            _dir: dir,
            path,
            c,
            binding,
            calendar: calendar_days,
            bars: bar_map,
        }
    }

    fn read(&self) -> Connection {
        crate::research::read::open_read_only(&self.path).unwrap()
    }

    /// Derive the dataset from the *stored* facts and bars, exactly as the
    /// future daily item will.
    fn derive(
        &self,
        source: &crate::security_master::suspensions::SuspensionSource,
    ) -> SuspensionSet {
        let facts = security::read_security_master(&self.c).unwrap();
        assemble_suspensions(source, &facts, &self.calendar, &self.bars).unwrap()
    }
}

/// A mid-gap confirmed fact: 2007-01-04..2007-01-10 (weekdays), announced the
/// day before.
fn confirmed_suspension() -> ConfirmedSuspensionFact {
    ConfirmedSuspensionFact {
        exchange: Exchange::Sz,
        code: "000631".into(),
        start_day: day(2007, 1, 4),
        end_day: day(2007, 1, 10),
        reason: SuspensionReason::ExchangeAnnouncement,
        known_at: Some(day(2007, 1, 3)),
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("ann-1".into()),
    }
}

/// AC-M2-8 end to end: with no admitted suspension source, the writer stores
/// the derived gap, and `tradability` answers `unknown`/`data_gap` for
/// 2006-04-28…2009-06-04 — never `suspended`.
#[test]
fn a_pending_source_stores_only_the_gap_and_tradability_stays_unknown() {
    let fixture = Fixture::new();
    let source = suspension_source(None);
    let set = fixture.derive(&source);
    assert_eq!(set.confirmed_rows, 0);
    assert_eq!(set.pending_sources.len(), 1);

    let row = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .expect("a non-empty suspension set registers");
    assert_eq!(row.dataset_kind, DATASET_KIND);
    assert_eq!(row.dataset_key, DATASET_KEY);
    assert_eq!(row.digest_format, DIGEST_FORMAT);
    assert_eq!(row.algorithm_version, ALGORITHM_VERSION);
    assert_eq!(row.record_count, 1);
    assert_eq!(
        row.first_day,
        crate::compact_market::epoch_day(day(2006, 4, 28))
    );
    assert_eq!(
        row.last_day,
        crate::compact_market::epoch_day(day(2009, 6, 4))
    );

    let stored = read_suspensions(&fixture.c).unwrap();
    assert_eq!(stored, set.rows);
    assert!(
        stored
            .iter()
            .all(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
    );

    let c = fixture.read();
    // The AC's range: the first, a middle and the last missing trading day.
    for probe in [day(2006, 4, 28), day(2007, 1, 5), day(2009, 6, 4)] {
        let report = tradability(&c, "sz.000631", Temporal::effective_only(probe)).unwrap();
        assert_eq!(
            report.state,
            Tradability::Unknown(TradabilityReason::DataGap),
            "{probe} is a data gap, not a suspension"
        );
        let suspension = report.suspension.expect("the gap row is the evidence");
        assert_eq!(suspension.certainty, SuspensionCertainty::UnresolvedGap);
        assert_eq!(suspension.reason, SuspensionReason::MissingBarNoEvent);
        assert_eq!(suspension.start_day, day(2006, 4, 28));
        assert_eq!(suspension.end_day, day(2009, 6, 4));
    }
    // The two bracketing bars are tradable.
    for probe in [day(2006, 4, 27), day(2009, 6, 5)] {
        let report = tradability(&c, "sz.000631", Temporal::effective_only(probe)).unwrap();
        assert_eq!(report.state, Tradability::Tradable, "{probe}");
    }
}

/// The confirmed branch of the seam: an admitted interval becomes one
/// `confirmed` row with the announcement day, splits the gap run, and is the
/// only path to `suspended` (§7.4 step 3). A `known_at` filter that predates
/// the announcement excludes both the confirmed row and the derived gaps
/// (fail-closed).
#[test]
fn an_admitted_source_stores_confirmed_rows_and_enables_suspended() {
    let fixture = Fixture::new();
    let fact = confirmed_suspension();
    let source = suspension_source(Some(ConfirmedSuspensionSnapshot {
        as_of: day(2026, 10, 26),
        facts: vec![fact],
    }));
    let set = fixture.derive(&source);
    assert_eq!(set.confirmed_rows, 1);
    assert_eq!(set.gap_rows, 2);
    assert!(set.pending_sources.is_empty());

    replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .expect("registration");

    let stored = read_suspensions(&fixture.c).unwrap();
    let confirmed = stored
        .iter()
        .find(|row| row.certainty == SuspensionCertainty::Confirmed)
        .expect("the admitted fact is stored");
    assert_eq!(confirmed.start_day, day(2007, 1, 4));
    assert_eq!(confirmed.end_day, day(2007, 1, 10));
    // Weekdays 01-04, 01-05, 01-08, 01-09, 01-10.
    assert_eq!(confirmed.missing_days, 5);
    assert_eq!(confirmed.reason, SuspensionReason::ExchangeAnnouncement);
    assert_eq!(confirmed.known_at, Some(day(2007, 1, 3)));
    assert_eq!(confirmed.source_record_id.as_deref(), Some("ann-1"));
    // The gaps are the stretches before and after the announced interval.
    let gaps: Vec<&crate::security_master::SuspensionInterval> = stored
        .iter()
        .filter(|row| row.certainty == SuspensionCertainty::UnresolvedGap)
        .collect();
    assert_eq!(
        (gaps[0].start_day, gaps[0].end_day),
        (day(2006, 4, 28), day(2007, 1, 3))
    );
    assert_eq!(
        (gaps[1].start_day, gaps[1].end_day),
        (day(2007, 1, 11), day(2009, 6, 4))
    );

    let c = fixture.read();
    let report = tradability(&c, "sz.000631", Temporal::effective_only(day(2007, 1, 5))).unwrap();
    assert_eq!(
        report.state,
        Tradability::NotTradable(TradabilityReason::Suspended)
    );
    // Known on 2007-01-03, not before: the same day asked with an earlier
    // known_at is `unknown`, because the confirmed row and the derived gaps
    // are both excluded.
    let report = tradability(
        &c,
        "sz.000631",
        Temporal::known_by(day(2007, 1, 5), day(2007, 1, 2)),
    )
    .unwrap();
    assert_eq!(report.state, Tradability::Unknown(TradabilityReason::NoBar));
}

/// An empty set removes the rows *and* the registration, so "written empty"
/// never looks like "never written" (§3.10; the DDL forbids a zero-row
/// registration).
#[test]
fn an_empty_set_removes_the_rows_and_the_registration() {
    let fixture = Fixture::new();
    let set = fixture.derive(&suspension_source(None));
    assert!(
        replace_suspensions(
            &fixture.c,
            &set,
            fixture.binding,
            "task-suspensions",
            "derived:suspensions",
        )
        .unwrap()
        .is_some()
    );
    assert!(read_registration(&fixture.c).unwrap().is_some());

    let empty = SuspensionSet::default();
    assert!(
        replace_suspensions(
            &fixture.c,
            &empty,
            fixture.binding,
            "task-suspensions",
            "derived:suspensions",
        )
        .unwrap()
        .is_none()
    );
    assert!(read_suspensions(&fixture.c).unwrap().is_empty());
    assert!(read_registration(&fixture.c).unwrap().is_none());
}

/// Rewriting the same set keeps the same `version_id` (content identity), and
/// a changed row changes it (§3.10).
#[test]
fn the_version_id_is_the_row_content_not_the_write_order() {
    let fixture = Fixture::new();
    let set = fixture.derive(&suspension_source(None));
    let first = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .unwrap();
    let second = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .unwrap();
    assert_eq!(first.version_id, second.version_id);
    assert_eq!(first.content_digest, second.content_digest);

    let mut changed = set.clone();
    changed.rows[0].end_day = changed.rows[0].end_day.pred_opt().unwrap();
    let third = replace_suspensions(
        &fixture.c,
        &changed,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .unwrap();
    assert_ne!(first.version_id, third.version_id);
}

/// The writer refuses a set whose counters do not describe its rows: the
/// evidence must not claim counts the table does not hold (H3).
#[test]
fn a_set_whose_counters_disagree_with_its_rows_is_refused() {
    let fixture = Fixture::new();
    let mut set = fixture.derive(&suspension_source(None));
    set.confirmed_rows = 1;
    let error = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("counters disagree"),
        "unexpected error: {error}"
    );
    // Nothing was written.
    assert!(read_suspensions(&fixture.c).unwrap().is_empty());
    assert!(read_registration(&fixture.c).unwrap().is_none());
}

/// XDM-134: the two skipped shapes write no row (the DDL forbids
/// `missing_days = 0`) but must not vanish. The dataset evidence carries both
/// counters and samples, and the skipped intervals neither split a gap nor add
/// one.
#[test]
fn skipped_confirmed_intervals_are_counted_in_evidence_and_write_no_row() {
    let fixture = Fixture::new();
    let weekend = ConfirmedSuspensionFact {
        exchange: Exchange::Sz,
        code: "000631".into(),
        start_day: day(2007, 1, 13),
        end_day: day(2007, 1, 14),
        reason: SuspensionReason::ExchangeAnnouncement,
        known_at: None,
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("ann-weekend".into()),
    };
    // The fixture calendar ends on 2009-06-05: 06-06..06-08 is not fully
    // covered, so it defers whole instead of writing 06-06 as a partial count.
    let past_the_end = ConfirmedSuspensionFact {
        exchange: Exchange::Sz,
        code: "000631".into(),
        start_day: day(2009, 6, 6),
        end_day: day(2009, 6, 8),
        reason: SuspensionReason::ExchangeAnnouncement,
        known_at: None,
        source_kind: "exchange_announcement".into(),
        source_record_id: Some("ann-past-end".into()),
    };
    let source = suspension_source(Some(ConfirmedSuspensionSnapshot {
        as_of: day(2009, 6, 9),
        facts: vec![weekend, past_the_end],
    }));

    let set = fixture.derive(&source);
    assert_eq!(set.confirmed_rows, 0);
    assert_eq!(set.skipped_no_trading_day, 1);
    assert_eq!(set.deferred_beyond_calendar, 1);
    assert_eq!(set.gap_rows, 1, "the skipped intervals split nothing");
    assert_eq!(set.skipped_samples.len(), 1);
    assert_eq!(set.deferred_samples.len(), 1);

    let written = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap()
    .expect("the gap row still registers");
    assert_eq!(written.record_count, 1);
    assert_eq!(read_suspensions(&fixture.c).unwrap(), set.rows);

    let (_kind, canonical) =
        evidence_store::read_evidence(&fixture.c, &written.evidence_digest).unwrap();
    let evidence: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["skipped_no_trading_day"], 1);
    assert_eq!(evidence["deferred_beyond_calendar"], 1);
    // The sample triple is what an operator needs to find the frozen page again.
    assert_eq!(evidence["skipped_samples"][0]["entity"], "sz.000631");
    assert_eq!(evidence["skipped_samples"][0]["start_day"], "2007-01-13");
    assert_eq!(evidence["skipped_samples"][0]["end_day"], "2007-01-14");
    assert_eq!(
        evidence["skipped_samples"][0]["source_record_id"],
        "ann-weekend"
    );
    assert_eq!(evidence["deferred_samples"][0]["entity"], "sz.000631");
    assert_eq!(evidence["deferred_samples"][0]["start_day"], "2009-06-06");
    assert_eq!(evidence["deferred_samples"][0]["end_day"], "2009-06-08");
    assert_eq!(
        evidence["deferred_samples"][0]["source_record_id"],
        "ann-past-end"
    );
}

/// The new skip counters get the same H3 treatment as the row counters: samples
/// cannot outnumber what the evidence says was skipped.
#[test]
fn a_set_whose_skip_samples_outnumber_their_counters_is_refused() {
    let fixture = Fixture::new();
    let mut set = fixture.derive(&suspension_source(None));
    set.deferred_samples
        .push(crate::security_master::suspensions::SkippedInterval {
            entity: "sz.000631".into(),
            start_day: day(2009, 6, 6),
            end_day: day(2009, 6, 7),
            source_record_id: None,
        });
    let error = replace_suspensions(
        &fixture.c,
        &set,
        fixture.binding,
        "task-suspensions",
        "derived:suspensions",
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("skip samples outnumber"),
        "unexpected error: {error}"
    );
    assert!(read_suspensions(&fixture.c).unwrap().is_empty());
    assert!(read_registration(&fixture.c).unwrap().is_none());
}
