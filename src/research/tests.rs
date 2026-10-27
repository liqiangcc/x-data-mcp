//! Research export tests (issue-383 §7, AC-M1-7/8/9/10).
//!
//! The fixture is built with the production write primitives
//! (`series::insert_series`/`insert_bars`, `derived::replace_adjustment_events`,
//! `calendar::commit_calendar`, `evidence_store`), not hand-written SQL, so the
//! rows the read model sees carry the same encoding the daily run produces.
//! Only a few things are poked with SQL on purpose: `volume_quantum` (M1 writes
//! NULL everywhere, so the column has to be proven wired with a non-NULL value),
//! the drift/cross-generation mutations, which simulate a concurrent daily run
//! without driving the whole task machinery, and one orphaned binding that
//! proves unprovable lineage is still rejected.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use chrono::{Duration, NaiveDate};
use rusqlite::{Connection, params};
use rust_decimal::Decimal;
use serde_json::json;

use super::read::{
    for_each_bar, for_each_calendar_day, for_each_factor, price_units, scaled_units,
};
use super::{
    AdjustmentSelection, DatasetSelection, EntitySelection, ExportRequest, plan, verify_unchanged,
};
use crate::compact_market::{
    CompactBar, Digest, EvidenceKind, ExcludedBar, SourceBinding, VersionEvidence,
};
use crate::market_data::{
    Adjustment, AdjustmentEvent, AdjustmentEventSet, AdjustmentEvidence, CalendarBatch,
    CalendarInputDates, CoverageStatus, EvidenceValue, Period, UnitCurrencyEvidence,
};
use crate::security_master::{SecurityLifecycleFact, SuspensionSet};
use crate::sqlite_market::calendar::{self, CalendarWrite};
use crate::sqlite_market::series::NewSeries;
use crate::sqlite_market::{derived, evidence_store, security, series};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_calendar::TradingCalendar;

const GEN1: &str = "gen-1";
const GEN2: &str = "gen-2";
const MANIFEST_SHA: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const CREATED_AT: &str = "2026-10-26T00:00:00Z";

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn days() -> Vec<NaiveDate> {
    vec![day(2026, 10, 20), day(2026, 10, 21), day(2026, 10, 22)]
}

/// Consecutive days starting at the fixture's first day. `count == 3` is the
/// `days()` window every non-size test uses.
fn history_days(count: usize) -> Vec<NaiveDate> {
    let first = day(2026, 10, 20);
    (0..count)
        .map(|offset| first + Duration::days(offset as i64))
        .collect()
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

fn digest(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// A complete `VersionEvidence` bundle: the store rejects anything else
/// (`check_version_evidence`), so the fixture cannot use a stub here.
fn version_evidence() -> serde_json::Value {
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

struct Fixture {
    /// Kept alive so the database file outlives the test.
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        Self::with_days(3)
    }

    /// A fixture whose stock series have `stock_day_count` bars. The index
    /// keeps the three-day `days()` window; the calendar follows the stock
    /// window so every stock bar is inside it.
    fn with_days(stock_day_count: usize) -> Self {
        let dir = tempdir();
        let path = dir.path().join("market.db");
        SqliteTasks::new(&path).initialize_compact().unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = c.transaction().unwrap();
        Self::populate(&tx, stock_day_count);
        tx.commit().unwrap();
        Self { _dir: dir, path }
    }

    fn populate(c: &Connection, stock_day_count: usize) {
        let stock_days = history_days(stock_day_count);
        let calendar_days = stock_days.clone();
        let generation_evidence = evidence_store::put_evidence(
            c,
            EvidenceKind::Generation,
            &json!({"generation_id": GEN1, "manifest_sha256": MANIFEST_SHA}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            c,
            &SourceBinding {
                generation_id: GEN1.into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation_evidence,
            },
        )
        .unwrap();
        let version_evidence =
            evidence_store::put_evidence(c, EvidenceKind::Version, &version_evidence()).unwrap();

        // Two stocks × two 口径, all bound to generation 1.
        let mut seed = 0x10;
        for (entity, base) in [("sh.600000", 1000i64), ("sz.000001", 2000i64)] {
            for adjustment in [Adjustment::Raw, Adjustment::Forward] {
                seed += 1;
                let price_scale = match adjustment {
                    Adjustment::Raw => 2,
                    Adjustment::Forward => 6,
                };
                let bars: Vec<CompactBar> = stock_days
                    .iter()
                    .copied()
                    .enumerate()
                    .map(|(index, day)| bar(day, base + index as i64))
                    .collect();
                let key = series::insert_series(
                    c,
                    &NewSeries {
                        entity: entity.into(),
                        period: Period::Day,
                        adjustment,
                        instrument_class: crate::market_data::INSTRUMENT_CLASS_STOCK,
                        price_scale,
                        content_digest: digest(seed),
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
                series::insert_bars(c, key, &bars).unwrap();
            }
        }
        // #381 option 1: the precision column exists and is NULL for ordinary
        // bars; one bar carries 100 to prove the export wires it through.
        c.execute(
            "UPDATE series_bars SET volume_quantum=100 WHERE series_key=(\
             SELECT series_key FROM market_series WHERE entity='sh.600000' AND adjustment=0)",
            [],
        )
        .unwrap();

        // One index benchmark series plus its unit/scope row (§6.3).
        let index_bars: Vec<CompactBar> = days()
            .into_iter()
            .map(|day| CompactBar {
                trade_day: crate::compact_market::epoch_day(day),
                open_units: 400_000,
                high_units: 401_000,
                low_units: 399_000,
                close_units: 400_500,
                volume: Some(1_000),
                amount: Some("123456.78".into()),
                r7_raw: None,
            })
            .collect();
        let index_key = series::insert_series(
            c,
            &NewSeries {
                entity: "sh.000300".into(),
                period: Period::Day,
                adjustment: Adjustment::Raw,
                instrument_class: crate::market_data::INSTRUMENT_CLASS_INDEX,
                price_scale: 2,
                content_digest: digest(0x31),
                record_count: index_bars.len() as i64,
                first_day: index_bars[0].trade_day,
                last_day: index_bars[index_bars.len() - 1].trade_day,
                coverage_status: CoverageStatus::Complete,
                source_binding_digest: binding,
                evidence_digest: version_evidence,
                last_event_seq: 1,
                created_at: CREATED_AT.into(),
                updated_at: CREATED_AT.into(),
            },
        )
        .unwrap();
        series::insert_bars(c, index_key, &index_bars).unwrap();
        let index_evidence = evidence_store::put_evidence(
            c,
            EvidenceKind::Derived,
            &json!({"fixture": "index units"}),
        )
        .unwrap();
        c.execute(
            "INSERT INTO index_series_meta(series_key,aliases,aggregate_scope,member_count,\
             price_unit,volume_unit,amount_unit,evidence_digest,evidence_note,created_at) \
             VALUES(?1,?2,?3,?4,'index_point','hand','yuan',?5,?6,?7)",
            params![
                index_key,
                "sz.399300",
                "constituents",
                300i64,
                index_evidence.as_slice(),
                "fixture: 0.01 point, hand, yuan",
                CREATED_AT
            ],
        )
        .unwrap();

        // One factor event per stock, through the production replace path.
        for entity in ["sh.600000", "sz.000001"] {
            let set = AdjustmentEventSet {
                entity: entity.into(),
                events: vec![AdjustmentEvent {
                    ordinal: 0,
                    effective_day: day(2026, 10, 21),
                    category: 1,
                    c_bits: [
                        f32::to_bits(3.5),
                        f32::to_bits(0.0),
                        f32::to_bits(3.0),
                        f32::to_bits(0.0),
                    ],
                    factors: [
                        Decimal::new(350, 2),
                        Decimal::ZERO,
                        Decimal::new(300, 2),
                        Decimal::ZERO,
                    ],
                    known_at: None,
                }],
                excluded: 0,
            };
            derived::replace_adjustment_events(c, &set, binding, "task-factor", "item-factor")
                .unwrap();
        }

        // The derived calendar, committed through its own gate.
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
        let coverage = calendar::measure_coverage(c, &calendar).unwrap();
        calendar::commit_calendar(
            c,
            &CalendarWrite {
                batch: &batch,
                coverage: &coverage,
                task_id: "task-calendar",
                item_id: "derived:calendar",
                generation_id: GEN1,
                generation_manifest_sha256: MANIFEST_SHA,
                generation_evidence: &json!({"generation_id": GEN1}),
            },
        )
        .unwrap();

        // Catalog head with both stocks as members.
        c.execute(
            "INSERT INTO catalog_versions(version_id,published_at,source,acquired_at,\
             compat_json,digest) VALUES('catalog-1',?1,'fixture',?1,'{}','digest')",
            params![CREATED_AT],
        )
        .unwrap();
        for (exchange, code, name) in [("sh", "600000", "浦发银行"), ("sz", "000001", "平安银行")]
        {
            c.execute(
                "INSERT INTO catalog_members(version_id,exchange,code,name,source_security_id,\
                 industry_status,industry_code,industry_name,industry_reason) \
                 VALUES('catalog-1',?1,?2,?3,'fixture','assigned','I','industry',NULL)",
                params![exchange, code, name],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO catalog_head(singleton,version_id) VALUES(1,'catalog-1')",
            [],
        )
        .unwrap();

        Self::populate_security_master(c, binding);
    }

    /// The M2 parsed-fact rows the export's security files carry (§12.4).
    ///
    /// Everything goes through the production writers, so the fixture cannot
    /// describe a shape the writers do not produce. `risk_warnings` is the one
    /// exception: it has no writer yet (#308 owns that path), so the rows are
    /// inserted directly — the export only needs rows to prove the file, and
    /// the DDL CHECKs are the contract until the writer exists.
    fn populate_security_master(c: &Connection, binding: Digest) {
        use crate::security_master::assembly::Assembly;
        use crate::security_master::{
            CapitalEvent, CapitalEventSet, Exchange, IssuerResolution, ListingSource,
            SuspensionCertainty, SuspensionInterval, SuspensionReason,
        };
        let window = days();
        let rows = ["sh.600000", "sz.000001"]
            .into_iter()
            .map(|entity| {
                let (exchange, code) = entity.split_once('.').unwrap();
                SecurityLifecycleFact {
                    exchange: if exchange == "sh" {
                        Exchange::Sh
                    } else {
                        Exchange::Sz
                    },
                    code: code.into(),
                    effective_from: window[0],
                    effective_to: None,
                    instrument_class: 0,
                    issuer_key: Some(format!("issuer-{code}")),
                    issuer_resolution: IssuerResolution::IssuerKey,
                    listing_date: Some(window[0]),
                    listing_date_tdx: Some(window[0]),
                    listing_source: ListingSource::ExchangeList,
                    b_listing_date: None,
                    delisting_date: None,
                    delisting_date_tdx: None,
                    delisting_source: crate::security_master::DelistingSource::Unknown,
                    delisting_reason_class: crate::security_master::DelistingReason::Unknown,
                    arrangement_start: None,
                    arrangement_end: None,
                    listing_conflict: false,
                    delisting_conflict: false,
                    known_at: Some(window[0]),
                }
            })
            .collect();
        security::replace_security_master(
            c,
            &Assembly {
                rows,
                migrations: Vec::new(),
                conflicts: Vec::new(),
                pending_sources: Vec::new(),
                unresolved: Vec::new(),
                exchange_as_of: Some(window[0]),
            },
            binding,
            "task-master",
            "derived:security_master",
        )
        .unwrap();

        security::replace_capital_events(
            c,
            &CapitalEventSet {
                entity: "sh.600000".into(),
                events: vec![
                    CapitalEvent::from_raw(
                        0,
                        window[0],
                        9,
                        [
                            f32::to_bits(0.0),
                            f32::to_bits(0.0),
                            f32::to_bits(100_000.0),
                            f32::to_bits(120_000.0),
                        ],
                    )
                    .unwrap(),
                    CapitalEvent::from_raw(
                        1,
                        window[1],
                        2,
                        [
                            f32::to_bits(100_000.0),
                            f32::to_bits(120_000.0),
                            f32::to_bits(100_000.0),
                            f32::to_bits(120_000.0),
                        ],
                    )
                    .unwrap(),
                ],
                excluded: 0,
                excluded_by_category: std::collections::BTreeMap::new(),
            },
            binding,
            "task-capital",
            "derived:capital_events",
        )
        .unwrap();

        for (code, status, raw) in [
            ("600000", crate::security_master::RiskStatus::Normal, "N"),
            (
                "000001",
                crate::security_master::RiskStatus::OtherRiskWarning,
                "ST",
            ),
        ] {
            c.execute(
                "INSERT INTO risk_warning_intervals(exchange,code,effective_from,effective_to,\
                 raw_status_code,normalized_status,announced_at,known_at,source_kind,\
                 source_record_id,source_document_sha256,source_generation_id,evidence_digest) \
                 VALUES(?1,?2,?3,NULL,?4,?5,NULL,NULL,'szse_name_history',NULL,NULL,NULL,?6)",
                params![
                    if code.starts_with('6') { "sh" } else { "sz" },
                    code,
                    crate::compact_market::epoch_day(window[0]),
                    raw,
                    status.as_str(),
                    binding.as_slice()
                ],
            )
            .unwrap();
        }

        let set = SuspensionSet {
            rows: vec![SuspensionInterval {
                exchange: Exchange::Sz,
                code: "000001".into(),
                start_day: window[1],
                end_day: window[1],
                certainty: SuspensionCertainty::UnresolvedGap,
                missing_days: 1,
                reason: SuspensionReason::MissingBarNoEvent,
                known_at: None,
                source_kind: "bar_evidence".into(),
                source_record_id: None,
            }],
            ..SuspensionSet::default()
        };
        crate::sqlite_market::suspensions::replace_suspensions(
            c,
            &set,
            binding,
            "task-suspensions",
            "derived:suspensions",
        )
        .unwrap();

        crate::sqlite_market::rules::replace_trading_rules(
            c,
            &crate::trading_rules::RuleBook {
                price_limits: vec![crate::trading_rules::PriceLimitRule {
                    rule_id: "fixture-main-board-10pct".into(),
                    market: crate::trading_rules::RuleMarket::All,
                    board: crate::trading_rules::RuleBoard::Main,
                    status_class: crate::trading_rules::StatusClass::Normal,
                    listing_phase: crate::trading_rules::ListingPhase::Seasoned,
                    band_bp: Some(1000),
                    band_side: crate::trading_rules::BandSide::Both,
                    rounding: crate::trading_rules::Rounding::HalfUp,
                    effective_from: window[0],
                    effective_to: None,
                    basis_ref: "fixture: main board 10%".into(),
                    basis_status: crate::trading_rules::BasisStatus::Verified,
                }],
                min_ticks: vec![crate::trading_rules::MinTickRule {
                    rule_id: "fixture-main-board-tick".into(),
                    market: crate::trading_rules::RuleMarket::All,
                    board: crate::trading_rules::RuleBoard::Main,
                    effective_from: window[0],
                    effective_to: None,
                    tick_units: 1,
                    price_scale: 2,
                    basis_ref: "fixture: 0.01 yuan".into(),
                    basis_status: crate::trading_rules::BasisStatus::Verified,
                }],
            },
            binding,
            "task-rules",
            "derived:trading_rules",
        )
        .unwrap();
    }

    fn read(&self) -> Connection {
        super::open_read_only(&self.path).unwrap()
    }

    fn write(&self) -> Connection {
        let c = Connection::open(&self.path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        c
    }

    /// Simulate the part of a daily run that changes a published series: one
    /// more bar and a new content digest/version id (the CHECK ties them).
    fn advance_series(&self, entity: &str, adjustment: Adjustment, seed: u8, new_day: NaiveDate) {
        let c = self.write();
        let key: i64 = c
            .query_row(
                "SELECT series_key FROM market_series WHERE entity=?1 AND adjustment=?2",
                params![entity, adjustment.code()],
                |r| r.get(0),
            )
            .unwrap();
        let content = digest(seed);
        let version_id: String = content.iter().map(|b| format!("{b:02x}")).collect();
        c.execute(
            "UPDATE market_series SET content_digest=?1,version_id=?2,\
             record_count=record_count+1,last_day=?3,updated_at=?4 WHERE series_key=?5",
            params![
                content.as_slice(),
                version_id,
                crate::compact_market::epoch_day(new_day),
                CREATED_AT,
                key
            ],
        )
        .unwrap();
        series::insert_bars(&c, key, &[bar(new_day, 1234)]).unwrap();
    }

    /// Point one series at a second generation, as a partially updated daily
    /// run would leave the database.
    fn move_series_to_generation_two(&self, entity: &str, adjustment: Adjustment) {
        let c = self.write();
        let evidence = evidence_store::put_evidence(
            &c,
            EvidenceKind::Generation,
            &json!({"generation_id": GEN2, "manifest_sha256": MANIFEST_SHA}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &c,
            &SourceBinding {
                generation_id: GEN2.into(),
                generation_manifest_sha256: [0x1b; 32],
                day_member_sha256: [0x1c; 32],
                security_event_sha256: [0x1e; 32],
                evidence_digest: evidence,
            },
        )
        .unwrap();
        let changed = c
            .execute(
                "UPDATE market_series SET source_binding_digest=?1 \
                 WHERE entity=?2 AND adjustment=?3",
                params![binding.as_slice(), entity, adjustment.code()],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Point one series at a digest with no `source_bindings` row, as a
    /// corrupted/legacy row would look. Turn the FK check off explicitly:
    /// the build may default it on, and the point is to simulate a row the
    /// planner must reject rather than to exercise SQLite's constraint.
    fn orphan_series_binding(&self, entity: &str, adjustment: Adjustment) {
        let c = Connection::open(&self.path).unwrap();
        c.pragma_update(None, "foreign_keys", false).unwrap();
        let orphan = [0xeeu8; 32];
        let changed = c
            .execute(
                "UPDATE market_series SET source_binding_digest=?1 \
                 WHERE entity=?2 AND adjustment=?3",
                params![orphan.as_slice(), entity, adjustment.code()],
            )
            .unwrap();
        assert_eq!(changed, 1);
    }

    /// Give the 前复权 bars 6dp values whose low digits vary, so the size test
    /// measures a qfq file shaped like production instead of a run of small
    /// integers that compresses unrealistically well.
    fn scramble_forward_prices(&self) {
        let c = self.write();
        let keys: Vec<i64> = c
            .prepare("SELECT series_key FROM market_series WHERE adjustment=?1")
            .unwrap()
            .query_map(params![Adjustment::Forward.code()], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for key in keys {
            let days: Vec<i64> = c
                .prepare("SELECT trade_day FROM series_bars WHERE series_key=?1 ORDER BY trade_day")
                .unwrap()
                .query_map(params![key], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ key as u64;
            for trade_day in days {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let close = 1_000_000 + ((state >> 16) % 900_000_000) as i64;
                let volume = 100 + ((state >> 8) % 100_000) as i64;
                let amount_cents = (close as i128 * volume as i128 / 10_000) as i64;
                let amount = format!("{}.{:02}", amount_cents / 100, amount_cents % 100);
                c.execute(
                    "UPDATE series_bars SET open_units=?1,high_units=?2,low_units=?3,\
                     close_units=?4,volume=?7,amount=?8 WHERE series_key=?5 AND trade_day=?6",
                    params![
                        close - 10,
                        close + 10,
                        close - 20,
                        close,
                        key,
                        trade_day,
                        volume,
                        amount
                    ],
                )
                .unwrap();
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn request(dataset: DatasetSelection) -> ExportRequest {
    ExportRequest {
        dataset,
        ..ExportRequest::default()
    }
}

/// A day **after** the fixture's last bar: `series_bars` is keyed by
/// `(series_key, trade_day)`, so a "new bar from the daily run" has to be one.
fn today() -> NaiveDate {
    day(2026, 10, 23)
}

#[test]
fn plan_binds_the_four_layers_and_is_deterministic() {
    let fixture = Fixture::new();
    let first = plan(&fixture.read(), &ExportRequest::default()).unwrap();
    let second = plan(&fixture.read(), &ExportRequest::default()).unwrap();

    // AC-M1-7: the same inputs produce the same identity.
    assert_eq!(first.snapshot_id, second.snapshot_id);
    assert!(first.snapshot_id.starts_with("snapshot-"));
    assert_eq!(first.snapshot_id.len(), "snapshot-".len() + 32);

    // Layer 1 (catalog), 3 (generation) and the series (layer 2).
    assert_eq!(first.catalog.selection, "catalog");
    assert_eq!(first.catalog.version_id.as_deref(), Some("catalog-1"));
    assert_eq!(first.catalog.member_count, 2);
    assert_eq!(first.generations.len(), 1);
    assert_eq!(first.generations[0].generation_id, GEN1);
    assert_eq!(first.generations[0].manifest_sha256, MANIFEST_SHA);
    let raw = first
        .series
        .iter()
        .find(|entry| entry.entity == "sh.600000" && entry.adjustment == Adjustment::Raw)
        .unwrap();
    assert_eq!(
        first.generation_of(&raw.source_binding_digest).unwrap(),
        &first.generations[0]
    );
    assert_eq!(first.series.len(), 5, "2 stocks × 2 口径 + 1 index");
    assert_eq!(
        first.derived.len(),
        6,
        "2 factor sets + 1 calendar + 3 whole-market M2 registrations"
    );
    assert_eq!(first.files.len(), 12, "5 M1 files + 7 M2 files");

    // The identity excludes the output facts and the timestamp.
    assert!(first.identity.get("created_at").is_none());
    assert!(first.identity.get("files").is_none());
    assert_eq!(first.identity["schema"], super::manifest::SCHEMA);
    assert_eq!(first.identity["generations"].as_array().unwrap().len(), 1);
    for entry in first.identity["series"].as_array().unwrap() {
        assert_eq!(entry["source_generation"]["generation_id"], GEN1);
    }

    // Deterministic ordering: stocks by entity then 口径, index last.
    let order: Vec<(&str, &str)> = first
        .series
        .iter()
        .map(|entry| (entry.entity.as_str(), entry.adjustment.as_str()))
        .collect();
    assert_eq!(
        order,
        vec![
            ("sh.600000", "raw"),
            ("sh.600000", "qfq"),
            ("sz.000001", "raw"),
            ("sz.000001", "qfq"),
            ("sh.000300", "raw"),
        ]
    );
    let index = first.series.last().unwrap();
    assert_eq!(index.instrument_class_name(), "index");
    assert_eq!(
        index.index.as_ref().unwrap().aggregate_scope,
        "constituents"
    );
    assert_eq!(index.index.as_ref().unwrap().excluded_bar_count, 0);
    assert_eq!(
        first.identity["series"].as_array().unwrap().last().unwrap()["excluded_bar_count"],
        json!(0)
    );
    assert_eq!(first.derived[0].dataset.as_str(), "adjustment_events");
    assert_eq!(first.derived[2].dataset.as_str(), "trading_calendar");
    assert_eq!(first.derived[2].algorithm_version, "cal-union-v1");
    // AC-M2-11: the exported M2 files name the registration they were
    // recomputed from.
    let datasets: Vec<&str> = first
        .derived
        .iter()
        .skip(3)
        .map(|entry| entry.dataset.as_str())
        .collect();
    assert_eq!(
        datasets,
        vec!["security_master", "suspensions", "price_limit_rules"]
    );
    for (entry, expected_key) in
        first
            .derived
            .iter()
            .skip(3)
            .zip(["a_share", "a_share", "price_limit_rules"])
    {
        assert_eq!(entry.key, expected_key);
        assert!(!entry.version_id.is_empty());
        assert!(first.generation_of(&entry.source_binding_digest).is_some());
    }
}

/// #383 owner ruling 2026-10-27: the export manifest carries how many index
/// bars the commit dropped, read from the same version evidence the MCP
/// metadata uses.
#[test]
fn manifest_reports_excluded_index_bars() {
    let fixture = Fixture::new();
    let c = fixture.write();
    let mut evidence = version_evidence();
    evidence["excluded_bars"] = serde_json::to_value(vec![ExcludedBar {
        date: day(2026, 10, 21),
        open: Decimal::new(400_000, 2),
        high: Decimal::new(399_000, 2),
        low: Decimal::new(399_000, 2),
        close: Decimal::new(401_000, 2),
    }])
    .unwrap();
    let digest = evidence_store::put_evidence(&c, EvidenceKind::Version, &evidence).unwrap();
    c.execute(
        "UPDATE market_series SET evidence_digest=?1 WHERE entity='sh.000300'",
        params![digest.as_slice()],
    )
    .unwrap();

    let planned = plan(&fixture.read(), &ExportRequest::default()).unwrap();
    let index = planned.series.last().unwrap();
    assert_eq!(index.index.as_ref().unwrap().excluded_bar_count, 1);
    assert_eq!(
        planned.identity["series"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["excluded_bar_count"],
        json!(1)
    );
}

#[test]
fn snapshot_id_changes_when_a_series_version_changes() {
    let fixture = Fixture::new();
    let before = plan(&fixture.read(), &ExportRequest::default()).unwrap();
    fixture.advance_series("sh.600000", Adjustment::Forward, 0x77, today());
    let after = plan(&fixture.read(), &ExportRequest::default()).unwrap();
    assert_ne!(before.snapshot_id, after.snapshot_id);
    // And the verifier notices the same change (AC-M1-8 core).
    let error = verify_unchanged(fixture.path(), &before).unwrap_err();
    assert!(
        error.to_string().contains("EXPORT_DRIFT"),
        "unexpected error: {error}"
    );
}

#[test]
fn cross_generation_export_records_each_series_generation() {
    let fixture = Fixture::new();
    fixture.move_series_to_generation_two("sz.000001", Adjustment::Forward);
    let snapshot = plan(&fixture.read(), &ExportRequest::default()).unwrap();

    // A daily run that crosses a generation boundary is exportable ...
    let ids: Vec<&str> = snapshot
        .generations
        .iter()
        .map(|generation| generation.generation_id.as_str())
        .collect();
    assert_eq!(ids, vec![GEN1, GEN2]);

    // ... and every series names the generation it came from.
    let series = snapshot.identity["series"].as_array().unwrap();
    let generation_of = |entity: &str, adjustment: &str| {
        series
            .iter()
            .find(|entry| {
                entry["entity"].as_str() == Some(entity)
                    && entry["adjustment"].as_str() == Some(adjustment)
            })
            .unwrap()["source_generation"]["generation_id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(generation_of("sh.600000", "raw"), GEN1);
    assert_eq!(generation_of("sh.600000", "qfq"), GEN1);
    assert_eq!(generation_of("sz.000001", "raw"), GEN1);
    assert_eq!(generation_of("sz.000001", "qfq"), GEN2);

    let moved = snapshot
        .series
        .iter()
        .find(|entry| entry.entity == "sz.000001" && entry.adjustment == Adjustment::Forward)
        .unwrap();
    assert_eq!(
        snapshot
            .generation_of(&moved.source_binding_digest)
            .unwrap()
            .generation_id,
        GEN2
    );
    // Derived datasets carry their own generation too.
    for entry in snapshot.identity["derived"].as_array().unwrap() {
        assert!(entry["source_generation"]["generation_id"].is_string());
    }
}

#[test]
fn unproven_lineage_is_rejected() {
    let fixture = Fixture::new();
    fixture.orphan_series_binding("sz.000001", Adjustment::Forward);
    let error = plan(&fixture.read(), &ExportRequest::default()).unwrap_err();
    assert!(
        error.to_string().contains("EXPORT_LINEAGE"),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("cannot be proven"));
}

#[test]
fn explicit_entities_replace_the_catalog_selection() {
    let fixture = Fixture::new();
    let explicit = ExportRequest {
        entities: EntitySelection::Explicit(vec!["sh.600000".into()]),
        ..ExportRequest::default()
    };
    let snapshot = plan(&fixture.read(), &explicit).unwrap();
    assert_eq!(snapshot.catalog.selection, "explicit");
    assert_eq!(snapshot.catalog.version_id, None);
    assert_eq!(snapshot.catalog.member_count, 1);
    let stocks: Vec<&str> = snapshot
        .series
        .iter()
        .filter(|entry| entry.instrument_class_name() == "stock")
        .map(|entry| entry.entity.as_str())
        .collect();
    assert_eq!(stocks, vec!["sh.600000", "sh.600000"]);
    // Index and calendar are registry-driven: an entity list never drops them.
    assert!(
        snapshot
            .series
            .iter()
            .any(|entry| entry.instrument_class_name() == "index")
    );
    assert!(
        snapshot
            .derived
            .iter()
            .any(|entry| entry.dataset.as_str() == "trading_calendar")
    );

    let missing = ExportRequest {
        entities: EntitySelection::Explicit(vec!["sh.600519".into()]),
        ..ExportRequest::default()
    };
    let error = plan(&fixture.read(), &missing).unwrap_err();
    assert!(
        error.to_string().contains("EXPORT_NOT_FOUND"),
        "unexpected error: {error}"
    );
}

#[test]
fn catalog_version_must_exist() {
    let fixture = Fixture::new();
    let pinned = ExportRequest {
        entities: EntitySelection::CatalogVersion("catalog-404".into()),
        ..ExportRequest::default()
    };
    let error = plan(&fixture.read(), &pinned).unwrap_err();
    assert!(error.to_string().contains("does not exist"), "{error}");
}

#[test]
fn a_dataset_without_rows_is_rejected() {
    let fixture = Fixture::new();
    let c = fixture.write();
    c.execute("DELETE FROM index_series_meta", []).unwrap();
    // `series_bars` references `market_series` without a cascade, so the bars
    // go first (the same order a real series removal would use).
    c.execute(
        "DELETE FROM series_bars WHERE series_key IN \
         (SELECT series_key FROM market_series WHERE instrument_class=?1)",
        params![crate::market_data::INSTRUMENT_CLASS_INDEX],
    )
    .unwrap();
    c.execute(
        "DELETE FROM market_series WHERE instrument_class=?1",
        params![crate::market_data::INSTRUMENT_CLASS_INDEX],
    )
    .unwrap();
    let error = plan(&fixture.read(), &request(DatasetSelection::Index)).unwrap_err();
    assert!(
        error.to_string().contains("EXPORT_EMPTY"),
        "unexpected error: {error}"
    );
}

#[test]
fn invalid_requests_are_rejected() {
    let fixture = Fixture::new();
    let reversed = ExportRequest {
        start: Some(day(2026, 10, 22)),
        end: Some(day(2026, 10, 20)),
        ..ExportRequest::default()
    };
    assert!(plan(&fixture.read(), &reversed).is_err());
    let meaningless = ExportRequest {
        dataset: DatasetSelection::Calendar,
        adjustment: AdjustmentSelection::Qfq,
        ..ExportRequest::default()
    };
    let error = plan(&fixture.read(), &meaningless).unwrap_err();
    assert!(error.to_string().contains("has no meaning"), "{error}");
    let bad_entity = ExportRequest {
        entities: EntitySelection::Explicit(vec!["600000".into()]),
        ..ExportRequest::default()
    };
    let error = plan(&fixture.read(), &bad_entity).unwrap_err();
    assert!(error.to_string().contains("non-canonical"), "{error}");
}

#[test]
fn the_export_connection_is_read_only() {
    let fixture = Fixture::new();
    let db_before = std::fs::read(fixture.path()).unwrap();
    let c = fixture.read();
    // AC-M1-9: `mode=ro` at the file layer and `query_only=1` at the connection
    // layer, so neither a statement nor a pragma can write.
    let query_only: i64 = c
        .query_row("PRAGMA query_only", [], |row| row.get(0))
        .unwrap();
    assert_eq!(query_only, 1);
    plan(&c, &ExportRequest::default()).unwrap();
    let write = c.execute(
        "UPDATE market_series SET updated_at='x' WHERE series_key=1",
        [],
    );
    assert!(write.is_err(), "a write must fail on the export connection");
    drop(c);
    assert_eq!(std::fs::read(fixture.path()).unwrap(), db_before);
}

#[test]
fn rows_stream_with_the_requested_range() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let snapshot = plan(&c, &ExportRequest::default()).unwrap();
    let raw = snapshot
        .series
        .iter()
        .find(|entry| entry.entity == "sh.600000" && entry.adjustment == Adjustment::Raw)
        .unwrap();
    let mut seen = Vec::new();
    let count = for_each_bar(
        &c,
        raw.series_key,
        Some(day(2026, 10, 21)),
        Some(day(2026, 10, 21)),
        |row| {
            seen.push((row.trade_day, row.volume_quantum, row.amount));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(seen[0].0, day(2026, 10, 21));
    assert_eq!(seen[0].1, Some(100), "volume_quantum is exported per bar");
    assert_eq!(seen[0].2, Some("100500.00".parse::<Decimal>().unwrap()));

    let mut factors = Vec::new();
    let count = for_each_factor(
        &c,
        "sh.600000",
        Some(day(2026, 10, 21)),
        Some(day(2026, 10, 21)),
        |row| {
            factors.push(row);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(count, 1);
    assert_eq!(factors[0].known_at, None);
    // gbbq-cat1-2dp-v1: a = 10/(10+3+0) = 10/13, b = 0
    assert_eq!(
        factors[0].factor_a,
        Decimal::new(10, 0) / Decimal::new(13, 0)
    );

    let mut calendar_days = Vec::new();
    let count = for_each_calendar_day(&c, Some(day(2026, 10, 21)), None, |day, sources| {
        calendar_days.push((day, sources));
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 2);
    assert_eq!(calendar_days[0].1, 3, "both inputs had a bar that day");
}

#[test]
fn decimals_are_scaled_exactly_once() {
    // A 2dp source widened to the 6dp price column: exact, no rounding.
    assert_eq!(
        scaled_units(Decimal::new(12345, 2), 6).unwrap(),
        123_450_000
    );
    // The amount column comes from an f32 expansion — f32(123456.71) is
    // 123456.7109375 — so the export must round to the column's 2dp.
    assert_eq!(
        scaled_units(Decimal::new(1_234_567_109_375, 7), 2).unwrap(),
        12_345_671
    );
    // A midpoint rounds away from zero: 123456.705 -> 123456.71. Banker's
    // rounding would give 123456.70, so this case tells the two apart.
    assert_eq!(
        scaled_units(Decimal::new(123_456_705, 3), 2).unwrap(),
        12_345_671
    );
    // A repeating decimal at 18dp: 10/13 rounds up at the 18th digit.
    assert_eq!(
        scaled_units(Decimal::new(10, 0) / Decimal::new(13, 0), 18).unwrap(),
        769_230_769_230_769_231
    );
    // Raw 2dp/6dp price units to the DECIMAL(18,6) column.
    assert_eq!(price_units(1_000, 2).unwrap(), 10_000_000);
    assert_eq!(price_units(1_000_000, 6).unwrap(), 1_000_000);
}

#[test]
fn the_public_read_surface_cannot_see_the_export_only_datasets() {
    let fixture = Fixture::new();
    let c = fixture.read();
    // AC-M1-10: the one visibility predicate the MCP read path uses.
    assert!(
        series::read_public_series(&c, "sh.000300", Period::Day)
            .unwrap()
            .is_none(),
        "index series must stay invisible to the MCP read surface"
    );
    let stock = series::read_public_series(&c, "sh.600000", Period::Day)
        .unwrap()
        .expect("the stock 前复权 series is visible");
    assert_eq!(stock.adjustment, Adjustment::Forward);
    assert!(
        series::read_public_series(&c, "sh.600000", Period::Year)
            .unwrap()
            .is_none()
    );
    // The export sees everything the MCP surface cannot.
    let snapshot = plan(&c, &ExportRequest::default()).unwrap();
    assert!(
        snapshot
            .series
            .iter()
            .any(|entry| entry.adjustment == Adjustment::Raw)
    );
    assert!(
        snapshot
            .series
            .iter()
            .any(|entry| entry.instrument_class_name() == "index")
    );
    assert!(!snapshot.derived.is_empty());
}

// ---------------------------------------------------------------------------
// Export (feature `export`): the Parquet writer, the manifest and the
// AC-M1-8/AC-M1-9 behaviours that need real files.
// ---------------------------------------------------------------------------

#[cfg(feature = "export")]
mod with_export {
    use super::*;
    use crate::research::export::{Compression, ExportOptions, SnapshotWriter, dry_run, export};

    fn options(root: &Path) -> ExportOptions {
        ExportOptions {
            out_root: root.to_path_buf(),
            compression: Compression::Snappy,
            row_group_rows: 2,
        }
    }

    fn snapshot_dir(root: &Path, id: &str) -> PathBuf {
        root.join(id)
    }

    #[test]
    fn export_writes_a_complete_snapshot() {
        let fixture = Fixture::new();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let directory = snapshot_dir(out.path(), &outcome.snapshot_id);
        assert_eq!(outcome.directory, directory);
        assert!(directory.join("manifest.json").is_file());
        for name in [
            super::super::DAILY_RAW_FILE,
            super::super::DAILY_QFQ_FILE,
            super::super::FACTORS_FILE,
            super::super::CALENDAR_FILE,
            super::super::INDEX_FILE,
            super::super::SECURITY_MASTER_FILE,
            super::super::SECURITY_NAMES_FILE,
            super::super::CAPITAL_EVENTS_FILE,
            super::super::RISK_WARNINGS_FILE,
            super::super::SUSPENSIONS_FILE,
            super::super::PRICE_LIMIT_RULES_FILE,
            super::super::UNIVERSE_INTERVALS_FILE,
        ] {
            assert!(directory.join(name).is_file(), "missing {name}");
        }
        assert!(
            !out.path()
                .join(format!(".tmp-{}", std::process::id()))
                .exists(),
            "the temporary directory must be gone"
        );

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(directory.join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["snapshot_id"], outcome.snapshot_id);
        assert_eq!(manifest["schema"], super::super::manifest::SCHEMA);
        assert_eq!(manifest["catalog"]["member_count"], 2);
        assert_eq!(manifest["generations"][0]["generation_id"], GEN1);
        assert_eq!(manifest["export"]["row_group_rows"], 2);
        // Run-dependent facts never enter the snapshot file (AC-M1-7).
        assert!(manifest.get("created_at").is_none());
        assert!(manifest["export"].get("started_at").is_none());
        assert!(manifest["export"].get("finished_at").is_none());
        assert!(manifest["export"].get("wal_bytes_delta").is_none());
        for entry in manifest["series"].as_array().unwrap() {
            assert_eq!(entry["source_generation"]["generation_id"], GEN1);
        }
        assert_eq!(
            manifest["limitations"].as_array().unwrap().len(),
            super::super::LIMITATIONS.len()
        );
        // Layer 4: every recorded sha256 is the file's sha256, and the row
        // count is what the export streamed.
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files.len(), 12);
        let mut total_rows = 0i64;
        for file in files {
            let path = directory.join(file["path"].as_str().unwrap());
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                file["sha256"].as_str().unwrap(),
                crate::hash::sha256(&bytes),
                "sha256 mismatch for {}",
                file["path"]
            );
            assert_eq!(file["bytes"].as_u64().unwrap(), bytes.len() as u64);
            total_rows += file["rows"].as_i64().unwrap();
        }
        // 2 stocks × 3 bars × 2 口径 + 2 factors + 3 calendar days + 3 index bars
        // + 2 master intervals + 0 names + 2 capital events + 2 risk warnings
        // + 1 suspension + 1 price-limit rule + 2 universe intervals
        assert_eq!(total_rows, 12 + 2 + 3 + 3 + 2 + 2 + 2 + 1 + 1 + 2);
        assert_eq!(outcome.files.len(), 12);
    }

    #[test]
    fn export_is_byte_reproducible() {
        let fixture = Fixture::new();
        let first_root = tempdir();
        let second_root = tempdir();
        let first = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(first_root.path()),
        )
        .unwrap();
        let second = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(second_root.path()),
        )
        .unwrap();
        assert_eq!(first.snapshot_id, second.snapshot_id);
        assert_eq!(first.files, second.files);
        assert!(!first.created_at.is_empty());

        // AC-M1-7: not just the id — every snapshot file, manifest included,
        // is byte-for-byte the same on a second export of the same database.
        for file in &first.files {
            let a = std::fs::read(first.directory.join(&file.path)).unwrap();
            let b = std::fs::read(second.directory.join(&file.path)).unwrap();
            assert_eq!(a, b, "{} differs between two exports", file.path);
        }
        let first_manifest = std::fs::read(first.directory.join("manifest.json")).unwrap();
        let second_manifest = std::fs::read(second.directory.join("manifest.json")).unwrap();
        assert_eq!(first_manifest, second_manifest, "manifest.json differs");
    }

    #[test]
    fn cross_generation_snapshot_writes_each_series_generation() {
        let fixture = Fixture::new();
        fixture.move_series_to_generation_two("sz.000001", Adjustment::Forward);
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(outcome.directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest["generations"].as_array().unwrap().len(), 2);
        let series = manifest["series"].as_array().unwrap();
        let generation_of = |entity: &str, adjustment: &str| {
            series
                .iter()
                .find(|entry| {
                    entry["entity"].as_str() == Some(entity)
                        && entry["adjustment"].as_str() == Some(adjustment)
                })
                .unwrap()["source_generation"]["generation_id"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(generation_of("sh.600000", "raw"), GEN1);
        assert_eq!(generation_of("sz.000001", "qfq"), GEN2);
    }

    #[test]
    fn export_refuses_to_overwrite_a_snapshot() {
        let fixture = Fixture::new();
        let out = tempdir();
        let first = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let error = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("EXPORT_OUTPUT_EXISTS"),
            "unexpected error: {error}"
        );
        assert!(snapshot_dir(out.path(), &first.snapshot_id).is_dir());
    }

    #[test]
    fn export_is_rejected_when_the_database_moves_under_it() {
        let fixture = Fixture::new();
        let out = tempdir();
        let mut writer = SnapshotWriter::create(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        writer.write_files().unwrap();
        // A daily run commits while the files are already on disk.
        fixture.advance_series("sz.000001", Adjustment::Forward, 0x55, today());
        let snapshot_id = writer.plan().snapshot_id.clone();
        let error = writer.finalize().unwrap_err();
        assert!(
            error.to_string().contains("EXPORT_DRIFT"),
            "unexpected error: {error}"
        );
        // AC-M1-8: no manifest, no snapshot directory, no temporary leftovers.
        assert!(!snapshot_dir(out.path(), &snapshot_id).exists());
        let leftovers: Vec<PathBuf> = std::fs::read_dir(out.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert!(leftovers.is_empty(), "leftovers: {leftovers:?}");
    }

    #[test]
    fn export_does_not_write_the_database() {
        let fixture = Fixture::new();
        let out = tempdir();
        let db_before = std::fs::read(fixture.path()).unwrap();
        export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        assert_eq!(std::fs::read(fixture.path()).unwrap(), db_before);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let fixture = Fixture::new();
        let out = tempdir();
        let (snapshot, manifest) = dry_run(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        assert_eq!(manifest["snapshot_id"], snapshot.snapshot_id);
        assert_eq!(manifest["files"].as_array().unwrap().len(), 12);
        assert!(manifest["files"][0]["sha256"].is_null());
        assert_eq!(std::fs::read_dir(out.path()).unwrap().count(), 0);
    }

    #[test]
    fn parquet_files_read_back_with_the_expected_schema() {
        use parquet::file::reader::{FileReader as _, SerializedFileReader};

        let fixture = Fixture::new();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let directory = snapshot_dir(out.path(), &outcome.snapshot_id);

        let check = |name: &str, columns: &[&str], rows: i64| {
            let reader =
                SerializedFileReader::new(std::fs::File::open(directory.join(name)).unwrap())
                    .unwrap();
            let metadata = reader.metadata();
            assert_eq!(metadata.file_metadata().num_rows(), rows, "{name} rows");
            // §7.6: snappy + format 1.0, the settings the size estimate in
            // §7.7 assumed. Lock them so a silent encoding change shows up.
            assert_eq!(metadata.file_metadata().version(), 1, "{name} format");
            for row_group in metadata.row_groups() {
                for column in row_group.columns() {
                    assert_eq!(
                        column.compression(),
                        parquet::basic::Compression::SNAPPY,
                        "{name} column compression"
                    );
                }
            }
            let schema = metadata.file_metadata().schema_descr();
            let names: Vec<&str> = schema
                .columns()
                .iter()
                .map(|column| column.name())
                .collect();
            assert_eq!(names, columns, "{name} columns");
        };
        check(super::super::DAILY_RAW_FILE, super::super::DAILY_COLUMNS, 6);
        check(super::super::DAILY_QFQ_FILE, super::super::DAILY_COLUMNS, 6);
        check(super::super::FACTORS_FILE, super::super::FACTOR_COLUMNS, 2);
        check(
            super::super::CALENDAR_FILE,
            super::super::CALENDAR_COLUMNS,
            3,
        );
        check(super::super::INDEX_FILE, super::super::INDEX_COLUMNS, 3);
        // The M2 files parse back with the columns the manifest lists (§12.4,
        // AC-M2-11). `security_names` has no writer yet, so its file is real
        // and empty rather than absent.
        check(
            super::super::SECURITY_MASTER_FILE,
            super::super::SECURITY_MASTER_COLUMNS,
            2,
        );
        check(
            super::super::SECURITY_NAMES_FILE,
            super::super::SECURITY_NAMES_COLUMNS,
            0,
        );
        check(
            super::super::CAPITAL_EVENTS_FILE,
            super::super::CAPITAL_EVENTS_COLUMNS,
            2,
        );
        check(
            super::super::RISK_WARNINGS_FILE,
            super::super::RISK_WARNINGS_COLUMNS,
            2,
        );
        check(
            super::super::SUSPENSIONS_FILE,
            super::super::SUSPENSIONS_COLUMNS,
            1,
        );
        check(
            super::super::PRICE_LIMIT_RULES_FILE,
            super::super::PRICE_LIMIT_RULES_COLUMNS,
            1,
        );
        check(
            super::super::UNIVERSE_INTERVALS_FILE,
            super::super::UNIVERSE_INTERVALS_COLUMNS,
            2,
        );

        // One row of the raw file, read back through the record API: the price
        // is the 2dp integer widened to 6dp, and volume_quantum survives.
        let reader = SerializedFileReader::new(
            std::fs::File::open(directory.join(super::super::DAILY_RAW_FILE)).unwrap(),
        )
        .unwrap();
        let row = reader.get_row_iter(None).unwrap().next().unwrap().unwrap();
        let fields: Vec<(&String, &parquet::record::Field)> = row.get_column_iter().collect();
        assert_eq!(
            fields[0].1,
            &parquet::record::Field::Str("sh.600000".into())
        );
        assert_eq!(
            fields[1].1,
            &parquet::record::Field::Date(
                crate::compact_market::epoch_day(day(2026, 10, 20)) as i32
            )
        );
        assert_eq!(
            fields[2].1,
            &parquet::record::Field::Decimal(parquet::data_type::Decimal::from_i64(
                9_900_000, 18, 6
            ))
        );
    }

    /// §7.7 (as corrected by M1 fix B): measure the daily files on a fixture
    /// with production-shaped 6dp prices. The design's 340MB assumed about
    /// 10 B/row for each file; the fixed-width qfq columns alone are more than
    /// 40 B/row, so the estimate was below the physical floor.
    #[test]
    fn daily_parquet_bytes_per_row_match_the_measured_fixture() {
        let fixture = Fixture::with_days(4000);
        fixture.scramble_forward_prices();
        let out = tempdir();
        let mut options = options(out.path());
        options.row_group_rows = 65_536;
        let outcome = export(fixture.path(), &ExportRequest::default(), &options).unwrap();
        let stat = |name: &str| outcome.files.iter().find(|file| file.path == name).unwrap();
        let raw = stat(super::super::DAILY_RAW_FILE);
        let qfq = stat(super::super::DAILY_QFQ_FILE);
        assert_eq!(raw.rows, 8_000, "2 stocks × 4000 bars");
        assert_eq!(qfq.rows, 8_000);
        let raw_per_row = raw.bytes as f64 / raw.rows as f64;
        let qfq_per_row = qfq.bytes as f64 / qfq.rows as f64;
        println!(
            "daily_raw {raw_per_row:.1} B/row, daily_qfq {qfq_per_row:.1} B/row \
             ({} / {} bytes)",
            raw.bytes, qfq.bytes
        );
        assert!(
            (20.0..=50.0).contains(&raw_per_row),
            "daily_raw {raw_per_row:.1} B/row is outside the measured range"
        );
        assert!(
            (30.0..=90.0).contains(&qfq_per_row),
            "daily_qfq {qfq_per_row:.1} B/row is outside the measured range"
        );
        // 340MB / 16,749,060 rows / 2 files ≈ 10 B/row per file. This fixture
        // has two entities and constant raw volume/amount, so it sits below
        // the production-shaped 55.9 B/row, but still far above 10.
        assert!(
            qfq_per_row > 25.0,
            "the qfq fixed-width floor should make 10 B/row impossible"
        );
    }

    /// CI hook: when `X_DATA_EXPORT_SMOKE_DIR` is set, leave a real export
    /// there so the job can read it with pyarrow/duckdb (a Rust-only test
    /// cannot prove a third-party reader accepts the files).
    #[test]
    fn the_m2_files_carry_dataset_temporal_and_their_rows() {
        let fixture = Fixture::new();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest::default(),
            &options(out.path()),
        )
        .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(outcome.directory.join("manifest.json")).unwrap(),
        )
        .unwrap();
        let files = manifest["files"].as_array().unwrap();
        let by_path = |name: &str| -> serde_json::Value {
            files
                .iter()
                .find(|entry| entry["path"] == name)
                .unwrap_or_else(|| panic!("{name} is not in the manifest"))
                .clone()
        };

        // §12.4 row counts, straight from the fixture's writers.
        for (name, rows, dataset, temporal) in [
            (
                super::super::SECURITY_MASTER_FILE,
                2,
                "market.security_master",
                (true, true, "partial"),
            ),
            (
                super::super::SECURITY_NAMES_FILE,
                0,
                "market.security_names",
                (true, true, "partial"),
            ),
            (
                super::super::CAPITAL_EVENTS_FILE,
                2,
                "market.capital_events",
                (false, false, "none"),
            ),
            (
                super::super::RISK_WARNINGS_FILE,
                2,
                "market.risk_warnings",
                (true, true, "partial"),
            ),
            (
                super::super::SUSPENSIONS_FILE,
                1,
                "market.suspensions",
                (true, true, "partial"),
            ),
            (
                super::super::PRICE_LIMIT_RULES_FILE,
                1,
                "market.price_limit_rules",
                (true, false, "none"),
            ),
            (
                super::super::UNIVERSE_INTERVALS_FILE,
                2,
                "market.universe",
                (true, true, "partial"),
            ),
        ] {
            let entry = by_path(name);
            assert_eq!(entry["rows"], json!(rows), "{name} rows");
            assert_eq!(entry["dataset"], dataset, "{name} dataset");
            assert_eq!(
                entry["temporal"]["as_of"],
                json!(temporal.0),
                "{name} as_of"
            );
            assert_eq!(
                entry["temporal"]["known_at"],
                json!(temporal.1),
                "{name} known_at"
            );
            assert_eq!(
                entry["temporal"]["known_at_coverage"], temporal.2,
                "{name} known_at_coverage"
            );
        }

        // The M1 files also name their dataset, and none of them claims a
        // temporal parameter it cannot honour.
        for (name, dataset) in [
            (super::super::DAILY_RAW_FILE, "market.daily"),
            (super::super::DAILY_QFQ_FILE, "market.daily"),
            (super::super::FACTORS_FILE, "market.daily_factors"),
            (super::super::CALENDAR_FILE, "market.calendar"),
            (super::super::INDEX_FILE, "market.index_daily"),
        ] {
            let entry = by_path(name);
            assert_eq!(entry["dataset"], dataset, "{name} dataset");
            assert_eq!(entry["temporal"]["as_of"], json!(false), "{name}");
            assert_eq!(entry["temporal"]["known_at"], json!(false), "{name}");
            assert_eq!(entry["temporal"]["known_at_coverage"], "none", "{name}");
        }

        // The three new limitations travel with the files (§12.4).
        let limitations = manifest["limitations"].as_array().unwrap();
        assert_eq!(limitations.len(), super::super::LIMITATIONS.len());
        assert!(limitations.iter().any(|text| {
            text.as_str()
                .unwrap()
                .starts_with("risk_warnings is incomplete by source")
        }));
        assert!(limitations.iter().any(|text| {
            text.as_str()
                .unwrap()
                .starts_with("capital_events.known_at is always NULL")
        }));
        assert!(limitations.iter().any(|text| {
            text.as_str()
                .unwrap()
                .starts_with("price_limit_rules rows with basis_status='pending'")
        }));
    }

    /// §12.4: `daily_raw` is the parsed fact, so it must not carry the derived
    /// limit columns. A consumer that wants a limit reads `price_limit_rules`
    /// and derives it, so the same `version_id` of the daily file cannot change
    /// when a rule changes (AC-M2-11).
    #[test]
    fn daily_raw_keeps_no_derived_columns() {
        for derived_column in ["limit_up", "limit_down", "limit_state", "rule_id"] {
            assert!(
                !super::super::DAILY_COLUMNS.contains(&derived_column),
                "daily_raw must not carry the derived column {derived_column}"
            );
        }
        let fixture = Fixture::new();
        let planned = plan(&fixture.read(), &ExportRequest::default()).unwrap();
        let raw = planned
            .files
            .iter()
            .find(|file| file.path == super::super::DAILY_RAW_FILE)
            .unwrap();
        assert_eq!(raw.columns, super::super::DAILY_COLUMNS);
        assert_eq!(raw.dataset, "market.daily");
        assert_eq!(raw.temporal, super::super::TemporalDecl::NONE);
    }

    /// `--dataset daily` stays exactly the two M1 bar files: the release smoke
    /// asserts that set, and the M2 files belong to their own selection.
    #[test]
    fn a_daily_only_snapshot_writes_no_m2_file() {
        let fixture = Fixture::new();
        let out = tempdir();
        let outcome = export(
            fixture.path(),
            &ExportRequest {
                dataset: DatasetSelection::Daily,
                ..ExportRequest::default()
            },
            &options(out.path()),
        )
        .unwrap();
        let mut paths: Vec<&str> = outcome
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            vec![super::super::DAILY_QFQ_FILE, super::super::DAILY_RAW_FILE]
        );
    }

    #[test]
    fn smoke_export_for_external_readers() {
        let Some(target) = std::env::var_os("X_DATA_EXPORT_SMOKE_DIR") else {
            return;
        };
        let target = PathBuf::from(target);
        std::fs::create_dir_all(&target).unwrap();
        let fixture = Fixture::new();
        let outcome = export(fixture.path(), &ExportRequest::default(), &options(&target)).unwrap();
        std::fs::write(target.join("snapshot-id.txt"), &outcome.snapshot_id).unwrap();
    }
}
