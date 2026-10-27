//! `price_limit` read-model tests (issue-391 §16 PR-5; AC-M2-9/AC-M2-10).
//!
//! The fixture writes through the production storage paths (security master,
//! series, calendar, rules) and then asks the read model. The rule values are
//! fixture data, not design constants: the same code answers 5% and 10% because
//! two rows say so, which is AC-M2-10.

use std::os::unix::fs::PermissionsExt;

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::json;

use super::*;
use crate::compact_market::{CompactBar, EvidenceKind, SourceBinding, VersionEvidence};
use crate::market_data::{
    Adjustment, AdjustmentEvidence, CalendarBatch, CalendarInputDates, CoverageStatus,
    EvidenceValue, Period, UnitCurrencyEvidence,
};
use crate::security_master::assembly::Assembly;
use crate::security_master::{
    DelistingReason, DelistingSource, Exchange, IssuerResolution, ListingSource,
    SecurityLifecycleFact,
};
use crate::sqlite_market::calendar::{self, CalendarWrite};
use crate::sqlite_market::rules::replace_trading_rules;
use crate::sqlite_market::series::NewSeries;
use crate::sqlite_market::{evidence_store, security, series};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_calendar::TradingCalendar;
use crate::trading_rules::{
    BandSide, BasisStatus, ListingPhase, MinTickRule, PriceLimitRule, Rounding, RuleBoard,
    RuleBook, RuleMarket, StatusClass,
};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// Ten weekdays around the R2 boundary (2026-07-06). 2026-07-04 is a Saturday
/// and is used as the non-trading-day witness.
fn days() -> Vec<NaiveDate> {
    vec![
        day(2026, 6, 29),
        day(2026, 6, 30),
        day(2026, 7, 1),
        day(2026, 7, 2),
        day(2026, 7, 3),
        day(2026, 7, 6),
        day(2026, 7, 7),
        day(2026, 7, 8),
        day(2026, 7, 9),
        day(2026, 7, 10),
    ]
}

fn bar(day: NaiveDate, close_units: i64) -> CompactBar {
    CompactBar {
        trade_day: crate::compact_market::epoch_day(day),
        open_units: close_units,
        high_units: close_units,
        low_units: close_units,
        close_units,
        volume: Some(100),
        amount: Some("100.00".into()),
        r7_raw: None,
    }
}

fn digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A complete `VersionEvidence` bundle: the store rejects anything else.
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

#[allow(clippy::too_many_arguments)]
fn lifecycle(
    exchange: Exchange,
    code: &str,
    listing: NaiveDate,
    instrument_class: u8,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: listing,
        effective_to: None,
        instrument_class,
        issuer_key: None,
        issuer_resolution: IssuerResolution::IssuerKey,
        listing_date: Some(listing),
        listing_date_tdx: Some(listing),
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
        known_at: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn rule(
    id: &str,
    market: RuleMarket,
    board: RuleBoard,
    status: StatusClass,
    phase: ListingPhase,
    band_bp: Option<i64>,
    side: BandSide,
    from: NaiveDate,
    to: Option<NaiveDate>,
) -> PriceLimitRule {
    PriceLimitRule {
        rule_id: id.into(),
        market,
        board,
        status_class: status,
        listing_phase: phase,
        band_bp,
        band_side: side,
        rounding: Rounding::HalfUp,
        effective_from: from,
        effective_to: to,
        basis_ref: format!("fixture:{id}"),
        basis_status: BasisStatus::Verified,
    }
}

fn tick(id: &str, market: RuleMarket, board: RuleBoard) -> MinTickRule {
    MinTickRule {
        rule_id: id.into(),
        market,
        board,
        effective_from: day(2020, 1, 1),
        effective_to: None,
        tick_units: 1,
        price_scale: 2,
        basis_ref: format!("fixture:{id}"),
        basis_status: BasisStatus::Verified,
    }
}

fn base_book() -> RuleBook {
    let start = day(2020, 1, 1);
    let mut pending = rule(
        "sz-main-st-first5-pending",
        RuleMarket::Sz,
        RuleBoard::Main,
        StatusClass::St,
        ListingPhase::First5Days,
        Some(500),
        BandSide::Both,
        day(2026, 1, 1),
        None,
    );
    pending.basis_status = BasisStatus::Pending;
    let mut book = RuleBook {
        price_limits: vec![
            rule(
                "sz-main-normal-v1",
                RuleMarket::Sz,
                RuleBoard::Main,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                Some(500),
                BandSide::Both,
                start,
                Some(day(2026, 7, 6)),
            ),
            rule(
                "sz-main-normal-v2",
                RuleMarket::Sz,
                RuleBoard::Main,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                Some(1_000),
                BandSide::Both,
                day(2026, 7, 6),
                None,
            ),
            rule(
                "sz-main-st",
                RuleMarket::Sz,
                RuleBoard::Main,
                StatusClass::St,
                ListingPhase::Seasoned,
                Some(500),
                BandSide::Both,
                start,
                None,
            ),
            rule(
                "sh-main-normal",
                RuleMarket::Sh,
                RuleBoard::Main,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                Some(1_000),
                BandSide::Both,
                start,
                None,
            ),
            rule(
                "sz-gem-normal",
                RuleMarket::Sz,
                RuleBoard::Gem,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                Some(2_000),
                BandSide::Both,
                start,
                None,
            ),
            rule(
                "sh-star-normal",
                RuleMarket::Sh,
                RuleBoard::Star,
                StatusClass::Normal,
                ListingPhase::Seasoned,
                Some(2_000),
                BandSide::Both,
                start,
                None,
            ),
            rule(
                "sh-star-star-st",
                RuleMarket::Sh,
                RuleBoard::Star,
                StatusClass::StarSt,
                ListingPhase::Seasoned,
                Some(1_000),
                BandSide::Both,
                start,
                None,
            ),
            rule(
                "sz-main-first-day",
                RuleMarket::Sz,
                RuleBoard::Main,
                StatusClass::All,
                ListingPhase::FirstDay,
                None,
                BandSide::None,
                start,
                None,
            ),
            rule(
                "sz-gem-first-day",
                RuleMarket::Sz,
                RuleBoard::Gem,
                StatusClass::All,
                ListingPhase::FirstDay,
                Some(2_000),
                BandSide::Both,
                start,
                None,
            ),
            pending,
        ],
        min_ticks: vec![
            tick("sz-main-tick", RuleMarket::Sz, RuleBoard::Main),
            tick("sz-gem-tick", RuleMarket::Sz, RuleBoard::Gem),
            tick("sh-main-tick", RuleMarket::Sh, RuleBoard::Main),
            tick("sh-star-tick", RuleMarket::Sh, RuleBoard::Star),
        ],
    };
    book.validate().unwrap();
    book
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    binding: crate::compact_market::Digest,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let path = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        let mut c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = c.transaction().unwrap();
        let days = days();

        let generation = evidence_store::put_evidence(
            &tx,
            EvidenceKind::Generation,
            &json!({"generation_id": "gen-1", "manifest_sha256": "fixture"}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &tx,
            &SourceBinding {
                generation_id: "gen-1".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        let version_evidence =
            evidence_store::put_evidence(&tx, EvidenceKind::Version, &series_evidence()).unwrap();

        let entities: Vec<(&str, Vec<usize>, i64)> = vec![
            ("sz.000001", (0..10).collect(), 1_000),
            ("sz.000002", (0..10).collect(), 2_000),
            ("sz.000003", (0..10).collect(), 3_000),
            ("sz.000005", (0..10).collect(), 5_000),
            ("sz.000006", (6..10).collect(), 6_000),
            ("sz.300001", (0..10).collect(), 7_000),
            ("sz.300002", vec![9], 7_500),
            ("sh.600519", (0..10).collect(), 8_000),
            ("sh.688001", (0..10).collect(), 9_000),
            ("sz.000004", vec![9], 4_000),
        ];
        let mut seed = 0x20u8;
        for (entity, indexes, base) in entities {
            seed += 1;
            let bars: Vec<CompactBar> = indexes
                .iter()
                .map(|index| bar(days[*index], base + *index as i64))
                .collect();
            let key = series::insert_series(
                &tx,
                &NewSeries {
                    entity: entity.into(),
                    period: Period::Day,
                    adjustment: Adjustment::Raw,
                    instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
                    price_scale: 2,
                    content_digest: digest(seed),
                    record_count: bars.len() as i64,
                    first_day: bars[0].trade_day,
                    last_day: bars[bars.len() - 1].trade_day,
                    coverage_status: CoverageStatus::Complete,
                    source_binding_digest: binding,
                    evidence_digest: version_evidence,
                    last_event_seq: 1,
                    created_at: "2026-07-10T00:00:00Z".into(),
                    updated_at: "2026-07-10T00:00:00Z".into(),
                },
            )
            .unwrap();
            series::insert_bars(&tx, key, &bars).unwrap();
        }

        let rows = vec![
            lifecycle(Exchange::Sz, "000001", days[0], 0),
            lifecycle(Exchange::Sz, "000002", days[0], 0),
            lifecycle(Exchange::Sz, "000003", days[0], 0),
            lifecycle(Exchange::Sz, "000004", days[9], 0),
            lifecycle(Exchange::Sz, "000005", days[0], 0),
            lifecycle(Exchange::Sz, "000006", days[6], 0),
            lifecycle(Exchange::Sz, "300001", days[0], 0),
            lifecycle(Exchange::Sz, "300002", days[9], 0),
            lifecycle(Exchange::Sh, "600519", days[0], 0),
            lifecycle(Exchange::Sh, "688001", days[0], 0),
        ];
        security::replace_security_master(
            &tx,
            &Assembly {
                rows,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(days[9]),
            },
            binding,
            "task-master",
            "derived:security_master",
        )
        .unwrap();

        let risk_evidence =
            evidence_store::put_evidence(&tx, EvidenceKind::Derived, &json!({"fixture": "risk"}))
                .unwrap();
        for (exchange, code, status) in [
            ("sz", "000001", "normal"),
            ("sz", "000003", "normal"),
            ("sz", "000004", "normal"),
            ("sz", "000005", "st"),
            ("sz", "000006", "st"),
            ("sz", "300001", "normal"),
            ("sz", "300002", "normal"),
            ("sh", "600519", "normal"),
            ("sh", "688001", "delisting_risk_warning"),
        ] {
            tx.execute(
                "INSERT INTO risk_warning_intervals(exchange,code,effective_from,effective_to,\
                 raw_status_code,normalized_status,announced_at,known_at,source_kind,\
                 source_record_id,source_document_sha256,source_generation_id,evidence_digest) \
                 VALUES(?1,?2,?3,NULL,?4,?4,NULL,NULL,'exchange_announcement','fixture',\
                 NULL,NULL,?5)",
                params![
                    exchange,
                    code,
                    crate::compact_market::epoch_day(days[0]),
                    status,
                    risk_evidence.as_slice()
                ],
            )
            .unwrap();
        }

        let batch = CalendarBatch {
            inputs: vec![
                CalendarInputDates {
                    entity: "sh.000001".into(),
                    member: "sh\\lday\\sh000001.day".into(),
                    day_member_sha256: "aa".repeat(32),
                    days: days.clone(),
                },
                CalendarInputDates {
                    entity: "sz.399001".into(),
                    member: "sz\\lday\\sz399001.day".into(),
                    day_member_sha256: "bb".repeat(32),
                    days: days.clone(),
                },
            ],
            security_event_sha256: crate::hash::sha256(&0u64.to_le_bytes()),
        };
        let calendar = TradingCalendar::union(&batch).unwrap();
        let coverage = calendar::measure_coverage(&tx, &calendar).unwrap();
        calendar::commit_calendar(
            &tx,
            &CalendarWrite {
                batch: &batch,
                coverage: &coverage,
                task_id: "task-calendar",
                item_id: "derived:calendar",
                generation_id: "gen-1",
                generation_manifest_sha256: "fixture",
                generation_evidence: &json!({"generation_id": "gen-1"}),
            },
        )
        .unwrap();

        replace_trading_rules(
            &tx,
            &base_book(),
            binding,
            "task-rules",
            "derived:price_limit_rules",
        )
        .unwrap();
        tx.commit().unwrap();
        Self {
            _dir: dir,
            path,
            binding,
        }
    }

    fn open(&self) -> Connection {
        let c = Connection::open(&self.path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        c
    }

    fn write_rules(&self, book: &RuleBook) {
        let c = self.open();
        replace_trading_rules(
            &c,
            book,
            self.binding,
            "task-rules",
            "derived:price_limit_rules",
        )
        .unwrap();
    }
}

fn ask(c: &Connection, entity: &str, as_of: NaiveDate) -> PriceLimitReport {
    price_limit(c, entity, Temporal::effective_only(as_of)).unwrap()
}

#[test]
fn a_seasoned_main_board_uses_the_rule_row_and_the_last_bar() {
    let fixture = Fixture::new();
    let c = fixture.open();
    // 000001: bars 1000..1009. as_of 2026-07-10 -> prev close 1008 (07-09).
    let report = ask(&c, "sz.000001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Known);
    assert_eq!(report.reason_code(), "known");
    assert_eq!(report.board, Some(Board::Main));
    assert_eq!(report.listing_phase, Some(ListingPhase::Seasoned));
    assert_eq!(report.status_class, Some(StatusClass::Normal));
    assert_eq!(report.rule_id.as_deref(), Some("sz-main-normal-v2"));
    assert_eq!(report.prev_close_units, Some(1_008));
    assert_eq!(report.limit_up_units, Some(1_109));
    assert_eq!(report.limit_down_units, Some(907));
    assert_eq!(report.algorithm_version, ALGORITHM_VERSION);
    assert!(report.rules_version.is_some());
}

#[test]
fn the_effective_date_selects_the_rule_row() {
    let fixture = Fixture::new();
    let c = fixture.open();
    // 2026-07-03 -> v1 (500 bp); prev close 1003 -> 1053 / 953.
    let before = ask(&c, "sz.000001", days()[4]);
    assert_eq!(before.rule_id.as_deref(), Some("sz-main-normal-v1"));
    assert_eq!(before.limit_up_units, Some(1_053));
    assert_eq!(before.limit_down_units, Some(953));

    // 2026-07-06 -> v2 (1000 bp); prev close 1004 -> 1104 / 904.
    let after = ask(&c, "sz.000001", days()[5]);
    assert_eq!(after.rule_id.as_deref(), Some("sz-main-normal-v2"));
    assert_eq!(after.limit_up_units, Some(1_104));
    assert_eq!(after.limit_down_units, Some(904));
}

#[test]
fn changing_the_rule_changes_the_answer_without_a_code_change() {
    let fixture = Fixture::new();
    let mut book = base_book();
    let v2 = book
        .price_limits
        .iter_mut()
        .find(|rule| rule.rule_id == "sz-main-normal-v2")
        .unwrap();
    v2.band_bp = Some(2_000);
    fixture.write_rules(&book);

    let report = ask(&fixture.open(), "sz.000001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Known);
    assert_eq!(report.band_bp, Some(2_000));
    // prev close 1008 -> 1209.6 rounds to 1210, 806.4 to 806.
    assert_eq!(report.limit_up_units, Some(1_210));
    assert_eq!(report.limit_down_units, Some(806));
}

#[test]
fn a_missing_rule_is_unknown_not_a_fallback_band() {
    let fixture = Fixture::new();
    fixture.write_rules(&RuleBook::default());
    let report = ask(&fixture.open(), "sz.000001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::RuleMissing));
    assert_eq!(report.limit_up_units, None);
    assert_eq!(report.limit_down_units, None);
}

#[test]
fn a_pending_rule_is_rule_pending() {
    let fixture = Fixture::new();
    // 000006 is ST, listed on days[6]; days[8] is its third trading day.
    let report = ask(&fixture.open(), "sz.000006", days()[8]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::RulePending));
    assert_eq!(report.listing_phase, Some(ListingPhase::First5Days));
    assert_eq!(report.status_class, Some(StatusClass::St));
}

#[test]
fn two_matching_rules_are_ambiguous() {
    let fixture = Fixture::new();
    let mut book = base_book();
    book.price_limits.push(rule(
        "all-seasoned",
        RuleMarket::All,
        RuleBoard::All,
        StatusClass::All,
        ListingPhase::Seasoned,
        Some(1_000),
        BandSide::Both,
        day(2019, 1, 1),
        None,
    ));
    fixture.write_rules(&book);
    let report = ask(&fixture.open(), "sz.000001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::RuleAmbiguous));
}

#[test]
fn an_unknown_st_status_lists_the_rule_table_candidates() {
    let fixture = Fixture::new();
    // 000002 has no risk interval, so its status is unknown. The candidates
    // must be the rows that exist, never a hardcoded {5%, 10%}.
    let report = ask(&fixture.open(), "sz.000002", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::StUnknown));
    assert_eq!(report.listing_phase, Some(ListingPhase::Seasoned));
    assert_eq!(report.candidates.len(), 2);
    let bands: Vec<Option<i64>> = report.candidates.iter().map(|c| c.band_bp).collect();
    assert!(bands.contains(&Some(1_000)));
    assert!(bands.contains(&Some(500)));
}

#[test]
fn a_first_day_rule_with_no_side_is_unbounded() {
    let fixture = Fixture::new();
    let report = ask(&fixture.open(), "sz.000004", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unbounded);
    assert_eq!(report.reason_code(), "unbounded");
    assert_eq!(report.band_side, Some(BandSide::None));
    assert_eq!(report.limit_up_units, None);
    assert_eq!(report.limit_down_units, None);
}

#[test]
fn a_first_day_without_an_earlier_bar_is_no_prev_close() {
    let fixture = Fixture::new();
    let report = ask(&fixture.open(), "sz.300002", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::NoPrevClose));
    assert_eq!(report.listing_phase, Some(ListingPhase::FirstDay));
}

#[test]
fn a_non_trading_day_is_not_answered_from_the_previous_day() {
    let fixture = Fixture::new();
    let report = ask(&fixture.open(), "sz.000001", day(2026, 7, 4));
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::NonTradingDay));
}

#[test]
fn a_star_board_st_uses_the_star_st_row() {
    let fixture = Fixture::new();
    let report = ask(&fixture.open(), "sh.688001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Known);
    assert_eq!(report.board, Some(Board::Star));
    assert_eq!(report.status_class, Some(StatusClass::StarSt));
    assert_eq!(report.band_bp, Some(1_000));
}

#[test]
fn the_gem_board_uses_its_own_rule() {
    let fixture = Fixture::new();
    let report = ask(&fixture.open(), "sz.300001", days()[9]);
    assert_eq!(report.state, PriceLimitState::Known);
    assert_eq!(report.board, Some(Board::Gem));
    assert_eq!(report.band_bp, Some(2_000));
}

#[test]
fn a_listing_day_rule_missing_for_the_board_is_unknown() {
    let fixture = Fixture::new();
    let mut book = base_book();
    book.price_limits
        .retain(|rule| rule.rule_id != "sz-gem-first-day");
    fixture.write_rules(&book);
    let report = ask(&fixture.open(), "sz.300002", days()[9]);
    assert_eq!(report.state, PriceLimitState::Unknown);
    assert_eq!(report.reason, Some(PriceLimitReason::RuleMissing));
}

#[test]
fn the_implementation_contains_no_band_or_tick_constants() {
    // The derived layer must not encode an amplitude. The rule values above
    // come from the fixture rows; this guards the implementation file only.
    let source = include_str!("../price_limit.rs");
    for forbidden in ["0.05", "0.10", "0.20", "1000"] {
        assert!(
            !source.contains(forbidden),
            "band/tick constant {forbidden} leaked into the derivation"
        );
    }
}

#[test]
fn the_fixture_hits_every_state_the_design_names() {
    let fixture = Fixture::new();
    let c = fixture.open();
    let known = ask(&c, "sz.000001", days()[9]);
    let unbounded = ask(&c, "sz.000004", days()[9]);
    let st_unknown = ask(&c, "sz.000002", days()[9]);
    assert_eq!(known.state, PriceLimitState::Known);
    assert_eq!(unbounded.state, PriceLimitState::Unbounded);
    assert_eq!(st_unknown.state, PriceLimitState::Unknown);
}

#[test]
fn rounding_matches_the_e12_half_up_definition() {
    // E-12's sample口径: the limit price is computed from the previous close
    // and the observed high is a hit only when it equals that price. The
    // rounding direction is the only variable here: 1005 × 1.1 = 1105.5.
    let mut tick = tick("sz-main-tick", RuleMarket::Sz, RuleBoard::Main);
    tick.tick_units = 1;
    for (rounding, expected_up, expected_down) in [
        (Rounding::HalfUp, 1_106, 905),
        (Rounding::Floor, 1_105, 904),
        (Rounding::Ceil, 1_106, 905),
    ] {
        let mut limit_rule = rule(
            "rounding",
            RuleMarket::Sz,
            RuleBoard::Main,
            StatusClass::Normal,
            ListingPhase::Seasoned,
            Some(1_000),
            BandSide::Both,
            day(2020, 1, 1),
            None,
        );
        limit_rule.rounding = rounding;
        let limits = super::compute_limit(1_005, 2, &limit_rule, &tick).unwrap();
        assert_eq!(limits.up, Some(expected_up), "{rounding:?} up");
        assert_eq!(limits.down, Some(expected_down), "{rounding:?} down");
    }
}
