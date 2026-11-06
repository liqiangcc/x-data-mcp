//! XDM-138: `replay --scope suspensions` end to end, in-crate.
//!
//! The fixture freezes the real A-3 pages into a source store and writes the
//! database inputs through the production writers (main table, calendar, raw
//! bars). The replay then re-parses the frozen bytes and must produce exactly
//! the rows the daily `derived:suspensions` writer produces from the same
//! inputs. Nothing here talks to the network.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{Datelike, NaiveDate};
use rusqlite::Connection;
use serde_json::json;

use super::replay::{
    COMPUTED_SUSPENSIONS_NO_DB, NOTHING_SELECTED, REBUILT_SUSPENSIONS, ReplayReport, ReplayRequest,
    SCOPE_NOT_READY, SUSPENSION_REPLAY_NOT_READY, Scope, ScopeReport, replay, suspension_summary,
};
use crate::compact_market::{CompactBar, Digest, EvidenceKind, SourceBinding, VersionEvidence};
use crate::exchange_source::request::{ExchangeRequest, PagingFormat, SSE_SOA_PARSER_VERSION};
use crate::exchange_source::{SourceId, index};
use crate::market_data::{
    Adjustment, AdjustmentEvidence, CalendarBatch, CalendarInputDates, CoverageStatus,
    EvidenceValue, INSTRUMENT_CLASS_STOCK, Period, UnitCurrencyEvidence,
};
use crate::security_master::assembly::Assembly;
use crate::security_master::suspensions::{
    SuspensionSet, assemble_suspensions, inputs_digest, suspension_source,
};
use crate::security_master::{
    DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};
use crate::source_store::{Registration, SourceStore, StoreKind};
use crate::sqlite_market::calendar::{self, CalendarWrite};
use crate::sqlite_market::series::{self, NewSeries};
use crate::sqlite_market::suspensions::{SuspensionInputs, SuspensionWrite};
use crate::sqlite_market::{evidence_store, security, suspensions};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_calendar::TradingCalendar;

const GEN: &str = "gen-138";
const MANIFEST_SHA: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const ACQUIRED_AT: &str = "2008-07-01T00:00:00Z";

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

fn lifecycle(code: &str) -> SecurityLifecycleFact {
    let from = day(2000, 1, 1);
    SecurityLifecycleFact {
        exchange: Exchange::Sh,
        code: code.into(),
        effective_from: from,
        effective_to: None,
        instrument_class: 0,
        issuer_key: None,
        issuer_resolution: IssuerResolution::CodeDate,
        listing_date: Some(from),
        listing_date_tdx: Some(from),
        listing_source: ListingSource::ExchangeList,
        b_listing_date: None,
        delisting_date: None,
        delisting_date_tdx: None,
        delisting_source: DelistingSource::Unknown,
        delisting_reason_class: DelistingReason::Unknown,
        arrangement_start: None,
        arrangement_end: None,
        listing_conflict: false,
        delisting_conflict: false,
        known_at: Some(from),
    }
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/exchange-a3")
        .join(name)
}

fn a3_fingerprint(start: &str, end: &str, page: u32) -> String {
    ExchangeRequest::new(
        SourceId::SseSuspension,
        crate::official_security::a3::SSE_SOA_URL,
    )
    .param(crate::official_security::a3::FROM_PARAM, start)
    .param(crate::official_security::a3::TO_PARAM, end)
    .paged(PagingFormat::SseSoaPageNo, page)
    .fingerprint()
}

fn register_page(
    store: &SourceStore,
    body: &[u8],
    start: &str,
    end: &str,
    page: u32,
    parser: &str,
) {
    let mut registration = Registration::new(
        SourceId::SseSuspension.as_str(),
        StoreKind::HttpFetch,
        a3_fingerprint(start, end, page),
        ACQUIRED_AT,
    );
    registration.parser_version = Some(parser.to_string());
    registration.basis_status = Some("approved_source".to_string());
    store.register(body, &registration).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    c: Connection,
    store: SourceStore,
}

impl Fixture {
    fn new() -> Self {
        Self::with_parser(SSE_SOA_PARSER_VERSION)
    }

    fn with_parser(parser: &str) -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let path = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        // `MarketBackend::open` acquires the runtime lease, which only accepts
        // a private (0600) database file.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
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

        let members = ["600001", "600018", "600680", "600849", "601607"];
        security::replace_security_master(
            &c,
            &Assembly {
                rows: members.iter().map(|code| lifecycle(code)).collect(),
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

        let calendar_days = weekdays(day(2008, 1, 1), day(2008, 6, 30));
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

        let suspended: BTreeMap<&str, Vec<NaiveDate>> = BTreeMap::from([
            (
                "600001",
                vec![day(2008, 5, 5), day(2008, 5, 6), day(2008, 5, 7)],
            ),
            ("600680", vec![day(2008, 5, 6)]),
            ("600018", vec![day(2008, 6, 2)]),
            (
                "600849",
                vec![day(2008, 5, 6), day(2008, 5, 7), day(2008, 5, 8)],
            ),
            ("601607", weekdays(day(2008, 6, 10), day(2008, 6, 30))),
        ]);
        for (index, code) in members.iter().enumerate() {
            let code = *code;
            let days: Vec<NaiveDate> = calendar_days
                .iter()
                .copied()
                .filter(|day| !suspended.get(code).is_some_and(|skip| skip.contains(day)))
                .collect();
            let bars: Vec<CompactBar> = days
                .iter()
                .map(|day| bar(*day, 1_000 + crate::compact_market::epoch_day(*day) % 100))
                .collect();
            let key = series::insert_series(
                &c,
                &NewSeries {
                    entity: format!("sh.{code}"),
                    period: Period::Day,
                    adjustment: Adjustment::Raw,
                    instrument_class: INSTRUMENT_CLASS_STOCK,
                    price_scale: 2,
                    content_digest: digest(0x31 + index as u8),
                    record_count: bars.len() as i64,
                    first_day: bars[0].trade_day,
                    last_day: bars[bars.len() - 1].trade_day,
                    coverage_status: CoverageStatus::Complete,
                    source_binding_digest: binding,
                    evidence_digest: version_evidence,
                    last_event_seq: 1,
                    created_at: ACQUIRED_AT.into(),
                    updated_at: ACQUIRED_AT.into(),
                },
            )
            .unwrap();
            series::insert_bars(&c, key, &bars).unwrap();
        }

        // Coverage needs at least one raw bar inside the calendar span, so the
        // fixture writes the bars before it commits the calendar.
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

        let store = SourceStore::new(dir.path().join("store"));
        register_page(
            &store,
            &std::fs::read(fixture_path("window_2008_q1.json")).unwrap(),
            "20080101",
            "20080331",
            1,
            parser,
        );
        register_page(
            &store,
            &std::fs::read(fixture_path("window_2008_q2_p1.json")).unwrap(),
            "20080401",
            "20080630",
            1,
            parser,
        );
        register_page(
            &store,
            &std::fs::read(fixture_path("window_2008_q2_p2.json")).unwrap(),
            "20080401",
            "20080630",
            2,
            parser,
        );

        Self {
            _dir: dir,
            path,
            c,
            store,
        }
    }

    fn request(&self, verify_only: bool) -> ReplayRequest<'_> {
        ReplayRequest::new(Some(Scope::Suspensions), verify_only, Some(&self.path))
    }

    fn selected_records(&self) -> Vec<crate::source_store::ManifestRecord> {
        self.store
            .records()
            .unwrap()
            .into_iter()
            .filter(|record| Scope::Suspensions.selects(record))
            .collect()
    }

    /// Write the dataset the way the daily `derived:suspensions` item does.
    fn write_daily_set(&self) -> SuspensionSet {
        let records = self.selected_records();
        let refs: Vec<&crate::source_store::ManifestRecord> = records.iter().collect();
        let rebuilt = index::rebuild(&self.store, &refs).unwrap();
        assert!(rebuilt.problems.is_empty(), "{:?}", rebuilt.problems);
        let inputs: SuspensionInputs = suspensions::read_inputs(&self.c).unwrap();
        let admitted = crate::official_security::a3::reparse(
            &self.store,
            &rebuilt.rows,
            &inputs.intervals,
            day(2008, 7, 1),
        )
        .unwrap();
        let source = suspension_source(Some(admitted.snapshot));
        let set = assemble_suspensions(
            &source,
            &inputs.intervals,
            &inputs.calendar,
            &inputs.bar_days,
        )
        .unwrap();
        let digest = inputs_digest(
            &inputs.master.version_id,
            &inputs.calendar,
            &inputs.bar_days,
        );
        suspensions::commit_suspensions(
            &self.c,
            &SuspensionWrite {
                set: &set,
                master: &inputs.master,
                task_id: "task-suspensions",
                item_id: "derived:suspensions",
                generation_id: GEN,
                generation_manifest_sha256: MANIFEST_SHA,
                member_sha256: &"0c".repeat(32),
                security_event_sha256: &"0e".repeat(32),
                generation_evidence: &json!({"generation_id": GEN, "manifest_sha256": MANIFEST_SHA}),
                replay_inputs_digest: Some(digest.as_str()),
            },
        )
        .unwrap();
        set
    }

    fn wipe_suspensions(&self) {
        self.c
            .execute("DELETE FROM suspension_intervals", [])
            .unwrap();
        self.c
            .execute(
                "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
                rusqlite::params![
                    crate::security_master::suspensions::DATASET_KIND,
                    crate::security_master::suspensions::DATASET_KEY
                ],
            )
            .unwrap();
    }
}

fn report_for(fixture: &Fixture, verify_only: bool) -> ReplayReport {
    replay(&fixture.store, &fixture.request(verify_only)).unwrap()
}

#[test]
fn without_a_database_the_scope_counts_the_frozen_facts_only() {
    let fixture = Fixture::new();
    let report = replay(
        &fixture.store,
        &ReplayRequest::new(Some(Scope::Suspensions), false, None),
    )
    .unwrap();
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "ok");
    assert_eq!(scope.rebuilt, COMPUTED_SUSPENSIONS_NO_DB);
    assert_eq!(scope.facts, Some(3));
    assert_eq!(scope.confirmed_rows, Some(0));
    assert!(scope.summary.is_none());
    assert!(scope.database.is_none());
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn replay_rebuilds_the_daily_suspension_rows_bit_for_bit() {
    let fixture = Fixture::new();
    let daily_set = fixture.write_daily_set();
    let daily_rows = suspensions::read_suspensions(&fixture.c).unwrap();
    assert_eq!(daily_rows.len(), daily_set.rows.len());
    let daily_summary = suspension_summary(&daily_rows);

    fixture.wipe_suspensions();
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );

    let report = report_for(&fixture, false);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "ok", "{report:?}");
    assert_eq!(scope.rebuilt, REBUILT_SUSPENSIONS);
    assert_eq!(scope.database, Some("match"));
    assert_eq!(scope.facts, Some(3));
    assert_eq!(scope.confirmed_rows, Some(3));
    assert_eq!(scope.summary.as_deref(), Some(daily_summary.as_str()));
    assert_eq!(
        suspensions::read_suspensions(&fixture.c).unwrap(),
        daily_rows,
        "the replay must write the same rows as the daily item"
    );
}

#[test]
fn verify_only_matches_then_reports_an_input_change() {
    let fixture = Fixture::new();
    fixture.write_daily_set();

    let report = report_for(&fixture, true);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "ok", "{report:?}");
    assert_eq!(scope.database, Some("match"));
    assert_eq!(scope.inputs_match, Some(true));
    assert!(scope.drift.is_empty());
    let inputs_before = scope.inputs_digest.clone();
    assert!(inputs_before.is_some());

    // The same store plus the same database state gives the same input digest.
    let again = report_for(&fixture, true);
    assert_eq!(again.scopes[0].inputs_digest, inputs_before);

    // Remove one trading day inside 600001's 2008-05-05..05-07 suspension:
    // the database advanced, so the recomputed `missing_days` legitimately
    // differs. This must be reported as changed inputs, not store corruption.
    fixture
        .c
        .execute(
            "DELETE FROM trading_days WHERE trade_day=?1",
            rusqlite::params![crate::compact_market::epoch_day(day(2008, 5, 7))],
        )
        .unwrap();

    let report = report_for(&fixture, true);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "drift");
    assert_eq!(scope.database, Some("drift"));
    assert_eq!(scope.drift_cause, Some("inputs_changed"));
    assert_eq!(scope.inputs_match, Some(false));
    assert_ne!(scope.inputs_digest, inputs_before);
    assert!(!scope.drift.is_empty());
    assert_eq!(suspensions::read_suspensions(&fixture.c).unwrap().len(), 5);
}

#[test]
fn a_database_without_a_calendar_is_suspension_replay_not_ready() {
    let fixture = Fixture::new();
    fixture.c.execute("DELETE FROM trading_days", []).unwrap();

    let report = report_for(&fixture, false);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, SUSPENSION_REPLAY_NOT_READY, "{report:?}");
    assert_eq!(report.status, SUSPENSION_REPLAY_NOT_READY);
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
    let problem = &report.problems[0];
    assert_eq!(problem.reason, SUSPENSION_REPLAY_NOT_READY);
}

#[test]
fn a_database_without_a_master_is_suspension_replay_not_ready() {
    let fixture = Fixture::new();
    fixture
        .c
        .execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            rusqlite::params![
                crate::security_master::assembly::DATASET_KIND,
                crate::security_master::assembly::DATASET_KEY
            ],
        )
        .unwrap();

    let report = report_for(&fixture, false);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, SUSPENSION_REPLAY_NOT_READY, "{report:?}");
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_unknown_parser_version_writes_nothing() {
    let fixture = Fixture::with_parser("exchange-sse-soa-page-v9");
    let report = report_for(&fixture, false);
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "unknown_parser");
    assert_ne!(scope.rebuilt, REBUILT_SUSPENSIONS);
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn an_empty_store_selects_nothing_and_never_opens_the_database() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let path = dir.path().join("absent.db");
    let report = replay(
        &store,
        &ReplayRequest::new(Some(Scope::Suspensions), false, Some(&path)),
    )
    .unwrap();
    let scope = &report.scopes[0];
    assert_eq!(scope.status, "store_empty");
    assert_eq!(scope.rebuilt, NOTHING_SELECTED);
    assert!(!path.exists());
}

// --- `--scope all` includes suspensions (T13) ------------------------------

fn all_request(fixture: &Fixture, verify_only: bool) -> ReplayRequest<'_> {
    ReplayRequest::new(None, verify_only, Some(&fixture.path))
}

fn suspensions_scope(report: &ReplayReport) -> &ScopeReport {
    let scope = report.scopes.last().unwrap();
    assert_eq!(scope.scope, "suspensions", "suspensions must be in `all`");
    scope
}

#[test]
fn all_has_six_scopes_and_suspensions_is_one_of_them() {
    let fixture = Fixture::new();
    let report = replay(&fixture.store, &all_request(&fixture, false)).unwrap();
    assert_eq!(report.scope, "all");
    let names: Vec<&str> = report.scopes.iter().map(|scope| scope.scope).collect();
    assert_eq!(
        names,
        [
            "industry",
            "exchange",
            "generation_facts",
            "risk_warnings",
            "security_names",
            "suspensions"
        ]
    );
}

#[test]
fn all_rebuilds_suspensions_exactly_like_the_standalone_scope() {
    let fixture = Fixture::new();
    fixture.write_daily_set();
    let daily_rows = suspensions::read_suspensions(&fixture.c).unwrap();
    fixture.wipe_suspensions();

    let all = replay(&fixture.store, &all_request(&fixture, false)).unwrap();
    let in_all = suspensions_scope(&all).clone();
    assert_eq!(in_all.status, "ok", "{all:?}");
    assert_eq!(in_all.rebuilt, REBUILT_SUSPENSIONS);
    assert_eq!(in_all.database, Some("match"));
    assert_eq!(
        suspensions::read_suspensions(&fixture.c).unwrap(),
        daily_rows
    );

    fixture.wipe_suspensions();
    let alone = report_for(&fixture, false);
    let alone = &alone.scopes[0];
    assert_eq!(in_all.summary, alone.summary);
    assert_eq!(in_all.inputs_digest, alone.inputs_digest);
    assert_eq!(in_all.inputs_match, alone.inputs_match);
    assert_eq!(in_all.facts, alone.facts);
    assert_eq!(in_all.confirmed_rows, alone.confirmed_rows);
    assert_eq!(in_all.gap_rows, alone.gap_rows);
    assert_eq!(in_all.missing_days, alone.missing_days);
    assert_eq!(
        suspensions::read_suspensions(&fixture.c).unwrap(),
        daily_rows
    );
}

#[test]
fn all_verify_only_matches_and_agrees_with_the_standalone_verdict() {
    let fixture = Fixture::new();
    fixture.write_daily_set();

    let all = replay(&fixture.store, &all_request(&fixture, true)).unwrap();
    let in_all = suspensions_scope(&all);
    assert_eq!(in_all.status, "ok", "{all:?}");
    assert_eq!(in_all.database, Some("match"));
    assert_eq!(in_all.inputs_match, Some(true));
    let alone = report_for(&fixture, true);
    assert_eq!(in_all.summary, alone.scopes[0].summary);
    assert_eq!(in_all.database, alone.scopes[0].database);
}

/// The scenario behind the old `inputs_changed`: the raw bars grew after the
/// dataset was written (the listed-ever first collection). The replay has to
/// keep reporting that, in `all` as much as alone.
#[test]
fn all_verify_only_still_finds_real_drift_when_the_bars_change() {
    let fixture = Fixture::new();
    fixture.write_daily_set();
    let before = report_for(&fixture, true).scopes[0].inputs_digest.clone();

    // Drop one bar of 600680 on a day that is not a known suspension: the
    // assembly now derives one more unresolved gap than the stored dataset has.
    let removed = fixture
        .c
        .execute(
            "DELETE FROM series_bars WHERE trade_day=?1 AND series_key IN \
             (SELECT series_key FROM market_series WHERE entity='sh.600680')",
            rusqlite::params![crate::compact_market::epoch_day(day(2008, 5, 20))],
        )
        .unwrap();
    assert_eq!(removed, 1);

    let all = replay(&fixture.store, &all_request(&fixture, true)).unwrap();
    let in_all = suspensions_scope(&all);
    assert_eq!(all.status, "drift", "{all:?}");
    assert_eq!(in_all.status, "drift");
    assert_eq!(in_all.database, Some("drift"));
    assert_eq!(in_all.drift_cause, Some("inputs_changed"));
    assert_eq!(in_all.inputs_match, Some(false));
    assert_ne!(in_all.inputs_digest, before);
    assert!(!in_all.drift.is_empty());
    // verify-only wrote nothing.
    assert_eq!(suspensions::read_suspensions(&fixture.c).unwrap().len(), 5);
}

#[test]
fn all_verify_only_still_finds_changed_rows_with_unchanged_inputs() {
    let fixture = Fixture::new();
    fixture.write_daily_set();
    fixture
        .c
        .execute(
            "DELETE FROM suspension_intervals WHERE rowid IN \
             (SELECT rowid FROM suspension_intervals LIMIT 1)",
            [],
        )
        .unwrap();

    let all = replay(&fixture.store, &all_request(&fixture, true)).unwrap();
    let in_all = suspensions_scope(&all);
    assert_eq!(all.status, "drift", "{all:?}");
    assert_eq!(in_all.drift_cause, Some("rows_changed"));
    assert_eq!(in_all.inputs_match, Some(true));
}

#[test]
fn all_with_a_database_missing_the_master_is_not_ready_but_not_a_failure() {
    let fixture = Fixture::new();
    fixture
        .c
        .execute(
            "DELETE FROM derived_dataset_versions WHERE dataset_kind=?1 AND dataset_key=?2",
            rusqlite::params![
                crate::security_master::assembly::DATASET_KIND,
                crate::security_master::assembly::DATASET_KEY
            ],
        )
        .unwrap();

    let all = replay(&fixture.store, &all_request(&fixture, false)).unwrap();
    let in_all = suspensions_scope(&all);
    assert_eq!(in_all.status, SCOPE_NOT_READY, "{all:?}");
    assert_eq!(in_all.rebuilt, "not_rebuilt");
    assert!(
        in_all
            .not_ready_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("security_master"))
    );
    assert_eq!(
        all.status, "ok",
        "a writing `all` must not fail a fresh init"
    );
    assert!(all.problems.is_empty(), "{:?}", all.problems);
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn all_with_a_database_missing_the_calendar_is_not_ready_but_not_a_failure() {
    let fixture = Fixture::new();
    fixture.c.execute("DELETE FROM trading_days", []).unwrap();

    let all = replay(&fixture.store, &all_request(&fixture, false)).unwrap();
    let in_all = suspensions_scope(&all);
    assert_eq!(in_all.status, SCOPE_NOT_READY, "{all:?}");
    assert!(
        in_all
            .not_ready_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("calendar"))
    );
    assert_eq!(all.status, "ok");
    assert!(
        suspensions::read_suspensions(&fixture.c)
            .unwrap()
            .is_empty()
    );
}

/// A verifying run that compared nothing must not pass, so `verify`/`--verify-only`
/// keep the stable failing code even inside `all`.
#[test]
fn all_verify_only_without_inputs_keeps_the_failing_code() {
    let fixture = Fixture::new();
    fixture.c.execute("DELETE FROM trading_days", []).unwrap();

    // Index the exchange pages first (a writing `all`, as the deploy hook does),
    // so the other scopes verify cleanly and only suspensions is left over.
    let written = replay(&fixture.store, &all_request(&fixture, false)).unwrap();
    assert_eq!(written.status, "ok", "{written:?}");

    let all = replay(&fixture.store, &all_request(&fixture, true)).unwrap();
    assert_eq!(suspensions_scope(&all).status, SCOPE_NOT_READY);
    assert_eq!(all.status, SUSPENSION_REPLAY_NOT_READY, "{all:?}");
    assert_eq!(all.problems[0].reason, SUSPENSION_REPLAY_NOT_READY);

    let verified =
        crate::source_store::replay::verify(&fixture.store, &all_request(&fixture, false)).unwrap();
    assert_eq!(verified.status, SUSPENSION_REPLAY_NOT_READY);
}

#[test]
fn the_standalone_scope_without_inputs_still_fails_closed() {
    let fixture = Fixture::new();
    fixture.c.execute("DELETE FROM trading_days", []).unwrap();
    let report = report_for(&fixture, false);
    assert_eq!(report.status, SUSPENSION_REPLAY_NOT_READY);
    assert_eq!(report.scopes[0].status, SUSPENSION_REPLAY_NOT_READY);
    assert!(report.scopes[0].not_ready_reason.is_none());
}

#[test]
fn all_on_an_empty_store_is_store_empty_and_never_opens_the_database() {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap();
    let store = SourceStore::new(dir.path().join("store"));
    let path = dir.path().join("absent.db");
    let report = replay(&store, &ReplayRequest::new(None, false, Some(&path))).unwrap();
    assert_eq!(report.status, "store_empty");
    assert_eq!(report.scopes.len(), 6);
    assert_eq!(report.scopes[5].status, "store_empty");
    assert_eq!(report.scopes[5].rebuilt, NOTHING_SELECTED);
    assert!(!path.exists());
}

#[test]
fn all_without_a_database_only_counts_the_frozen_facts() {
    let fixture = Fixture::new();
    let report = replay(&fixture.store, &ReplayRequest::new(None, false, None)).unwrap();
    let scope = suspensions_scope(&report);
    assert_eq!(scope.status, "ok");
    assert_eq!(scope.rebuilt, COMPUTED_SUSPENSIONS_NO_DB);
    assert_eq!(scope.facts, Some(3));
}
