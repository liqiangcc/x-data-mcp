//! M2 read-model tests (issue-391 §16 PR-4; AC-M2-4/6/8).
//!
//! The fixture is written through the production write paths
//! (`security::replace_security_master`, `series::insert_series`/`insert_bars`,
//! `calendar::commit_calendar`, `security::replace_capital_events`), so the
//! rows the read model sees carry the same encoding the daily run produces.
//! Two tables are not driven through their writer here: `risk_warning_intervals`
//! has no writer in M2 (it needs #308), and the `suspension_intervals` rows are
//! inserted with SQL so the read model is pinned independently of the
//! derivation and its writer (`sqlite_market::suspensions`, PR-6); those paths
//! are covered end to end by that module's tests.
//!
//! The ten-day fixture is built so every §11.1 rule has a witness:
//!
//! | entity | witness |
//! |---|---|
//! | `sz.000001` | plain member, risk interval with `known_at` |
//! | `sz.000002` | listed on day 2, `known_at` after day 1 |
//! | `sz.000003` | delisted on day 7 |
//! | `sh.600018` | code reuse: old interval + `ambiguous` current interval |
//! | `sz.300114`/`sz.302132` | candidate migration: both codes carry the same bars |
//! | `sz.000631` | bar gap, `unresolved_gap` (AC-M2-8) |
//! | `sz.000004` | confirmed suspension |
//! | `sz.000005` | unknown listing date, membership by bar evidence |
//! | `sh.600519` | bars before its listing date (has bar, not a member) |
//! | `sh.600028` | A+H issuer with capital events (AC-M2-6) |
//! | `sh.000300` | non-A instrument class, never a member |

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use chrono::NaiveDate;
use rusqlite::{Connection, params};
use serde_json::json;

use super::market_cap::{
    MarketCapOptions, MarketCapReason, MarketCapState, Reconciliation, ShareOptions, ShareReason,
    ShareScope, shares_at,
};
use super::member_state::{MemberFlag, Membership, NotMemberReason};
use super::temporal::{KnownAtCoverage, Temporal};
use super::tradability::{Tradability, TradabilityReason, tradability};
use super::universe::{UniverseRequest, member_state, universe};
use crate::compact_market::{CompactBar, EvidenceKind, SourceBinding, VersionEvidence};
use crate::market_data::{
    Adjustment, AdjustmentEvidence, CalendarBatch, CalendarInputDates, CoverageStatus,
    EvidenceValue, Period, UnitCurrencyEvidence,
};
use crate::security_master::assembly::{Assembly, CodeMigrationFact, MigrationCertainty};
use crate::security_master::{
    CapitalEvent, CapitalEventSet, DelistingReason, DelistingSource, Exchange, IssuerResolution,
    ListingSource, RiskStatus, SecurityLifecycleFact,
};
use crate::sqlite_market::calendar::{self, CalendarWrite};
use crate::sqlite_market::series::NewSeries;
use crate::sqlite_market::{evidence_store, security, series};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_calendar::TradingCalendar;

const GEN1: &str = "gen-1";
const MANIFEST_SHA: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";
const CREATED_AT: &str = "2026-10-26T00:00:00Z";

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

/// Ten consecutive trading days (2006-10-09 … 2006-10-20, weekdays only).
fn days() -> Vec<NaiveDate> {
    vec![
        day(2006, 10, 9),
        day(2006, 10, 10),
        day(2006, 10, 11),
        day(2006, 10, 12),
        day(2006, 10, 13),
        day(2006, 10, 16),
        day(2006, 10, 17),
        day(2006, 10, 18),
        day(2006, 10, 19),
        day(2006, 10, 20),
    ]
}

/// Index of the code-reuse boundary (600018's current subject).
const REUSE_DAY: usize = 5;
/// Index of the candidate migration day (300114 -> 302132).
const MIGRATION_DAY: usize = 6;
/// Index of 000003's delisting day.
const DELIST_DAY: usize = 7;

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

/// A complete `VersionEvidence` bundle: the store rejects anything else, so the
/// fixture cannot use a stub here.
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
    from: NaiveDate,
    to: Option<NaiveDate>,
    listing: Option<NaiveDate>,
    delisting: Option<NaiveDate>,
    issuer_resolution: IssuerResolution,
    instrument_class: u8,
    known_at: Option<NaiveDate>,
) -> SecurityLifecycleFact {
    SecurityLifecycleFact {
        exchange,
        code: code.into(),
        effective_from: from,
        effective_to: to,
        instrument_class,
        issuer_key: None,
        issuer_resolution,
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
        known_at,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    /// Which days each entity has a 不复权 bar on.
    bars: BTreeMap<String, BTreeSet<NaiveDate>>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(true)
    }

    fn without_calendar() -> Self {
        Self::build(false)
    }

    fn build(with_calendar: bool) -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/dev/shm")
            .unwrap();
        let path = dir.path().join("market.db");
        let tasks = SqliteTasks::new(&path);
        tasks.initialize_compact().unwrap();
        let mut c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let tx = c.transaction().unwrap();
        let days = days();
        let mut bars: BTreeMap<String, BTreeSet<NaiveDate>> = BTreeMap::new();

        let generation_evidence = evidence_store::put_evidence(
            &tx,
            EvidenceKind::Generation,
            &json!({"generation_id": GEN1, "manifest_sha256": MANIFEST_SHA}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &tx,
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
            evidence_store::put_evidence(&tx, EvidenceKind::Version, &series_evidence()).unwrap();

        // 不复权 daily series per entity. The bar days are the witness lists.
        let series_bars: Vec<(&str, Vec<usize>, i64)> = vec![
            ("sz.000001", (0..10).collect(), 1000),
            ("sz.000002", (2..10).collect(), 2000),
            ("sz.000003", (0..DELIST_DAY).collect(), 3000),
            ("sh.600018", (0..10).collect(), 6000),
            ("sz.300114", (0..10).collect(), 1100),
            ("sz.302132", (0..10).collect(), 1200),
            ("sz.000631", vec![0, 9], 6310),
            (
                "sz.000004",
                (0..10).filter(|index| !(3..=4).contains(index)).collect(),
                4000,
            ),
            ("sz.000005", (2..10).collect(), 5000),
            ("sh.600519", (0..10).collect(), 5190),
            ("sh.600028", (0..10).collect(), 1000),
        ];
        let mut seed = 0x10u8;
        for (entity, indexes, base) in series_bars {
            seed += 1;
            let bars_on: Vec<CompactBar> = indexes
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
                    record_count: bars_on.len() as i64,
                    first_day: bars_on[0].trade_day,
                    last_day: bars_on[bars_on.len() - 1].trade_day,
                    coverage_status: CoverageStatus::Complete,
                    source_binding_digest: binding,
                    evidence_digest: version_evidence,
                    last_event_seq: 1,
                    created_at: CREATED_AT.into(),
                    updated_at: CREATED_AT.into(),
                },
            )
            .unwrap();
            series::insert_bars(&tx, key, &bars_on).unwrap();
            bars.insert(
                entity.to_string(),
                indexes.into_iter().map(|index| days[index]).collect(),
            );
        }

        // The assembled main table.
        let reuse = days[REUSE_DAY];
        let rows = vec![
            lifecycle(
                Exchange::Sz,
                "000001",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                Some(days[0]),
            ),
            lifecycle(
                Exchange::Sz,
                "000002",
                days[2],
                None,
                Some(days[2]),
                None,
                IssuerResolution::IssuerKey,
                0,
                Some(days[2]),
            ),
            lifecycle(
                Exchange::Sz,
                "000003",
                days[0],
                None,
                Some(days[0]),
                Some(days[DELIST_DAY]),
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sh,
                "600018",
                day(2000, 7, 19),
                Some(reuse),
                Some(day(2000, 7, 19)),
                None,
                IssuerResolution::CodeDate,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sh,
                "600018",
                reuse,
                None,
                Some(reuse),
                None,
                IssuerResolution::Ambiguous,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sz,
                "300114",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sz,
                "302132",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sz,
                "000631",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sz,
                "000004",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                Some(days[0]),
            ),
            lifecycle(
                Exchange::Sz,
                "000005",
                days[0],
                None,
                None,
                None,
                IssuerResolution::CodeDate,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sh,
                "600519",
                days[0],
                None,
                Some(days[4]),
                None,
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sh,
                "600028",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::IssuerKey,
                0,
                None,
            ),
            lifecycle(
                Exchange::Sh,
                "000300",
                days[0],
                None,
                Some(days[0]),
                None,
                IssuerResolution::CodeDate,
                1,
                None,
            ),
        ];
        let migrations = vec![CodeMigrationFact {
            from_exchange: Exchange::Sz,
            from_code: "300114".into(),
            to_exchange: Exchange::Sz,
            to_code: "302132".into(),
            effective_from: days[MIGRATION_DAY],
            certainty: MigrationCertainty::Candidate,
            source_kind: "bar_evidence".into(),
            source_record_id: None,
            known_at: None,
        }];
        security::replace_security_master(
            &tx,
            &Assembly {
                rows,
                migrations,
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

        // A+H capital facts for 600028: 1.0e9 float A shares, 1.2e9 company
        // total (the difference is the H shares).
        security::replace_capital_events(
            &tx,
            &CapitalEventSet {
                entity: "sh.600028".into(),
                events: vec![
                    CapitalEvent::from_raw(
                        0,
                        days[0],
                        9,
                        [
                            f32::to_bits(0.0),
                            f32::to_bits(0.0),
                            f32::to_bits(100_000.0),
                            f32::to_bits(120_000.0),
                        ],
                    )
                    .unwrap(),
                ],
                excluded: 0,
                excluded_by_category: BTreeMap::new(),
            },
            binding,
            "task-capital",
            "derived:capital_events",
        )
        .unwrap();

        // Risk warnings: 000001 is proven `normal` from day 0 with a known_at;
        // every other entity has no interval, so its status is `unknown`.
        let evidence = evidence_store::put_evidence(
            &tx,
            EvidenceKind::Derived,
            &json!({"fixture": "risk warnings"}),
        )
        .unwrap();
        tx.execute(
            "INSERT INTO risk_warning_intervals(exchange,code,effective_from,effective_to,\
             raw_status_code,normalized_status,announced_at,known_at,source_kind,\
             source_record_id,source_document_sha256,source_generation_id,evidence_digest) \
             VALUES('sz','000001',?1,NULL,'normal','normal',?2,?2,'exchange_announcement',\
             'fixture-1',NULL,NULL,?3)",
            params![
                crate::compact_market::epoch_day(days[0]),
                days[0].to_string(),
                evidence.as_slice()
            ],
        )
        .unwrap();

        // Suspensions: a derived gap for 000631 and a confirmed one for 000004.
        tx.execute(
            "INSERT INTO suspension_intervals(exchange,code,start_day,end_day,certainty,\
             missing_days,reason,known_at,source_kind,source_record_id,evidence_digest) \
             VALUES('sz','000631',?1,?2,'unresolved_gap',8,'missing_bar_no_event',NULL,\
             'fixture','gap-1',?3)",
            params![
                crate::compact_market::epoch_day(days[1]),
                crate::compact_market::epoch_day(days[8]),
                evidence.as_slice()
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO suspension_intervals(exchange,code,start_day,end_day,certainty,\
             missing_days,reason,known_at,source_kind,source_record_id,evidence_digest) \
             VALUES('sz','000004',?1,?2,'confirmed',2,'exchange_announcement',?3,\
             'exchange_announcement','ann-1',?4)",
            params![
                crate::compact_market::epoch_day(days[3]),
                crate::compact_market::epoch_day(days[4]),
                days[2].to_string(),
                evidence.as_slice()
            ],
        )
        .unwrap();

        if with_calendar {
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
                    generation_id: GEN1,
                    generation_manifest_sha256: MANIFEST_SHA,
                    generation_evidence: &json!({"generation_id": GEN1}),
                },
            )
            .unwrap();
        }

        tx.commit().unwrap();
        Self {
            _dir: dir,
            path,
            bars,
        }
    }

    fn read(&self) -> Connection {
        super::read::open_read_only(&self.path).unwrap()
    }

    /// Promote the fixture's candidate migration to confirmed. The interval
    /// bounds are deliberately left stale: both codes still have an open
    /// interval from day 0, so the read model itself must return only one of
    /// them on each side of the migration day.
    fn with_confirmed_migration() -> Self {
        let fixture = Self::build(true);
        let c = Connection::open(&fixture.path).unwrap();
        c.execute(
            "UPDATE code_migrations SET certainty='confirmed' WHERE from_exchange='sz' AND from_code='300114'",
            [],
        )
        .unwrap();
        fixture
    }

    fn bar_entities(&self, as_of: NaiveDate) -> BTreeSet<String> {
        self.bars
            .iter()
            .filter(|(_, days)| days.contains(&as_of))
            .map(|(entity, _)| entity.clone())
            .collect()
    }
}

fn entities(universe: &super::universe::Universe) -> Vec<&str> {
    universe
        .members
        .iter()
        .map(|member| member.entity.as_str())
        .collect()
}

/// AC-M2-4: ten historical days, the member set, and every difference from
/// "has a bar that day" classified by reason.
#[test]
fn universe_matches_the_bar_evidence_set_on_ten_historical_days() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();
    // The expected member set per day, written out rather than derived from the
    // implementation so the test can disagree with it.
    let expected: Vec<Vec<&str>> = vec![
        // day 0: 000002/000005 are not members yet
        vec![
            "sh.600018",
            "sh.600028",
            "sz.000001",
            "sz.000003",
            "sz.000004",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 1
        vec![
            "sh.600018",
            "sh.600028",
            "sz.000001",
            "sz.000003",
            "sz.000004",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 2: 000002 lists, 000005 becomes a member on bar evidence
        vec![
            "sh.600018",
            "sh.600028",
            "sz.000001",
            "sz.000002",
            "sz.000003",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 3
        vec![
            "sh.600018",
            "sh.600028",
            "sz.000001",
            "sz.000002",
            "sz.000003",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 4: 600519 lists (its bars on days 0..3 predate the listing)
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000003",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 5: 600018's current subject starts (still exactly one member)
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000003",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 6: the candidate migration day
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000003",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 7: 000003 delists
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 8
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
        // day 9
        vec![
            "sh.600018",
            "sh.600028",
            "sh.600519",
            "sz.000001",
            "sz.000002",
            "sz.000004",
            "sz.000005",
            "sz.000631",
            "sz.300114",
            "sz.302132",
        ],
    ];
    // Members without a bar, per day: `sz.000631` only has bars on days 0 and 9,
    // and `sz.000004` is suspended on days 3/4. Both stay in the universe.
    let expected_member_without_bar: Vec<usize> = vec![0, 1, 1, 2, 2, 1, 1, 1, 1, 0];
    // Bars before the listing date: 600519 on days 0..3.
    let expected_bar_without_member: Vec<usize> = vec![1, 1, 1, 1, 0, 0, 0, 0, 0, 0];

    for (index, as_of) in days.iter().enumerate() {
        let request = UniverseRequest::new(*as_of);
        let answer = universe(&c, &request).unwrap();
        assert_eq!(entities(&answer), expected[index], "members on {as_of}");
        assert_eq!(answer.as_of, *as_of);
        assert!(answer.master_version.is_some(), "master version on {as_of}");

        let bar_entities = fixture.bar_entities(*as_of);
        let members: BTreeSet<String> = answer
            .members
            .iter()
            .map(|member| member.entity.clone())
            .collect();
        let mut member_without_bar = Vec::new();
        let mut bar_without_member = Vec::new();
        for member in &answer.members {
            if !bar_entities.contains(&member.entity) {
                member_without_bar.push(member.entity.clone());
            }
        }
        for entity in &bar_entities {
            if !members.contains(entity) {
                bar_without_member.push(entity.clone());
            }
        }
        assert_eq!(
            member_without_bar.len(),
            expected_member_without_bar[index],
            "members without a bar on {as_of}"
        );
        assert_eq!(
            bar_without_member.len(),
            expected_bar_without_member[index],
            "bars without membership on {as_of}"
        );
        // Every difference carries a reason, and the reason is the documented
        // one: a missing bar is a suspension or a data gap, and a bar before
        // the listing date is `before_listing`.
        for entity in &member_without_bar {
            let report = tradability(&c, entity, Temporal::effective_only(*as_of)).unwrap();
            let expected_state = match entity.as_str() {
                // The confirmed suspension row covers days 3 and 4.
                "sz.000004" => Tradability::NotTradable(TradabilityReason::Suspended),
                // The interval is `unresolved_gap`: a missing bar is a data gap,
                // never a suspension (AC-M2-8).
                "sz.000631" => Tradability::Unknown(TradabilityReason::DataGap),
                other => panic!("unexpected member without a bar: {other}"),
            };
            assert_eq!(report.state, expected_state, "{entity} on {as_of}");
        }
        for entity in &bar_without_member {
            let state = member_state(&c, entity, &request).unwrap().unwrap();
            assert_eq!(
                state.membership.reason(),
                Some(NotMemberReason::BeforeListing),
                "{entity} on {as_of}"
            );
        }
    }
}

#[test]
fn a_suspended_member_stays_in_the_universe() {
    let fixture = Fixture::new();
    let c = fixture.read();
    // 000004 has no bar on its confirmed suspension days and is still a
    // member: membership and tradability are different questions (§11.1).
    let answer = universe(&c, &UniverseRequest::new(days()[3])).unwrap();
    let member = answer
        .members
        .iter()
        .find(|member| member.entity == "sz.000004")
        .expect("a suspended member is still a member");
    assert!(member.has_series);
    assert_eq!(member.status, RiskStatus::Unknown);
    assert!(member.flags.contains(&MemberFlag::DelistingUnknown));
}

#[test]
fn strict_drops_only_the_two_ambiguity_cases() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();

    // Day 0: 302132's copied history overlaps 300114, and 600018 is still on
    // its code-dated (non-ambiguous) interval.
    let strict = universe(&c, &UniverseRequest::new(days[0]).strict(true)).unwrap();
    assert!(!entities(&strict).contains(&"sz.302132"));
    assert!(entities(&strict).contains(&"sh.600018"));
    assert_eq!(strict.exclusions.strict_candidate_overlap, 1);
    assert_eq!(strict.exclusions.strict_ambiguous_issuer, 0);
    let lenient = universe(&c, &UniverseRequest::new(days[0])).unwrap();
    assert!(entities(&lenient).contains(&"sz.302132"));
    let overlap = lenient
        .members
        .iter()
        .find(|member| member.entity == "sz.302132")
        .unwrap();
    assert!(
        overlap
            .flags
            .contains(&MemberFlag::CandidateMigrationOverlap)
    );

    // Day 5: 600018's current subject is ambiguous; the old interval ended on
    // the same day, so strict drops the code entirely. 302132 is still inside
    // the candidate overlap.
    let strict = universe(&c, &UniverseRequest::new(days[REUSE_DAY]).strict(true)).unwrap();
    assert!(!entities(&strict).contains(&"sh.600018"));
    assert!(!entities(&strict).contains(&"sz.302132"));
    assert_eq!(strict.exclusions.strict_ambiguous_issuer, 1);
    assert_eq!(strict.exclusions.strict_candidate_overlap, 1);

    // Day 6: the candidate migration day passed, so the new code is kept.
    let strict = universe(&c, &UniverseRequest::new(days[MIGRATION_DAY]).strict(true)).unwrap();
    assert!(entities(&strict).contains(&"sz.302132"));
    assert!(!entities(&strict).contains(&"sh.600018"));
}

#[test]
fn a_confirmed_migration_returns_one_code_with_canonical_and_aliases() {
    let fixture = Fixture::with_confirmed_migration();
    let c = fixture.read();
    let days = days();

    // Before the migration: only the old code, canonical already the new code.
    let before = universe(&c, &UniverseRequest::new(days[MIGRATION_DAY - 1])).unwrap();
    assert!(entities(&before).contains(&"sz.300114"));
    assert!(!entities(&before).contains(&"sz.302132"));
    let old = before
        .members
        .iter()
        .find(|member| member.entity == "sz.300114")
        .unwrap();
    assert_eq!(old.canonical_entity, "sz.302132");
    assert_eq!(old.aliases, vec!["sz.300114".to_owned()]);

    // On/after the migration day: only the new code, same canonical identity.
    let after = universe(&c, &UniverseRequest::new(days[MIGRATION_DAY])).unwrap();
    assert!(!entities(&after).contains(&"sz.300114"));
    assert!(entities(&after).contains(&"sz.302132"));
    let new = after
        .members
        .iter()
        .find(|member| member.entity == "sz.302132")
        .unwrap();
    assert_eq!(new.canonical_entity, "sz.302132");
    assert_eq!(new.aliases, vec!["sz.300114".to_owned()]);

    // The whole interval file carries the same identity on both sides.
    let intervals = super::universe::member_intervals(&c).unwrap();
    for interval in intervals
        .iter()
        .filter(|row| row.entity == "sz.300114" || row.entity == "sz.302132")
    {
        assert_eq!(interval.canonical_entity, "sz.302132");
        assert_eq!(interval.aliases, vec!["sz.300114".to_owned()]);
    }
}

#[test]
fn a_candidate_migration_does_not_create_aliases() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let answer = universe(&c, &UniverseRequest::new(days()[0])).unwrap();
    for entity in ["sz.300114", "sz.302132"] {
        let member = answer
            .members
            .iter()
            .find(|member| member.entity == entity)
            .unwrap();
        assert_eq!(member.canonical_entity, entity);
        assert!(member.aliases.is_empty());
    }
}

#[test]
fn known_at_excludes_null_rows_and_counts_them() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();
    let request = UniverseRequest::new(days[1]).with_known_at(days[1]);
    let answer = universe(&c, &request).unwrap();
    // 000001 and 000004 carry a known_at by day 1.
    assert_eq!(entities(&answer), vec!["sz.000001", "sz.000004"]);
    assert_eq!(answer.exclusions.excluded_by_known_at, 10);
    let report = answer.temporal.dataset("security_master").unwrap();
    assert_eq!(report.rows, 12);
    assert_eq!(report.unknown_known_at_rows, 9);
    assert_eq!(report.excluded_known_at_rows, 10);
    assert_eq!(report.coverage(), KnownAtCoverage::Partial);
    assert_eq!(answer.known_at, Some(days[1]));

    // Same `as_of`, two information sets: 000002 lists on day 2 and its row is
    // known on day 2, so a day-1 cutoff drops it by `known_at` rather than by
    // `as_of` — the two filters are not interchangeable (§12.2).
    let answer = universe(&c, &UniverseRequest::new(days[2]).with_known_at(days[1])).unwrap();
    assert!(!entities(&answer).contains(&"sz.000002"));
    assert_eq!(answer.exclusions.excluded_by_known_at, 10);
    let answer = universe(&c, &UniverseRequest::new(days[2]).with_known_at(days[2])).unwrap();
    assert!(entities(&answer).contains(&"sz.000002"));
    assert_eq!(answer.exclusions.excluded_by_known_at, 9);
}

#[test]
fn risk_status_is_unknown_without_a_covering_interval() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let request = UniverseRequest::new(days()[2]);
    let answer = universe(&c, &request).unwrap();
    let proven = answer
        .members
        .iter()
        .find(|member| member.entity == "sz.000001")
        .unwrap();
    assert_eq!(proven.status, RiskStatus::Normal);
    let uncovered = answer
        .members
        .iter()
        .find(|member| member.entity == "sz.000002")
        .unwrap();
    assert_eq!(uncovered.status, RiskStatus::Unknown);

    // Under a known_at filter the risk interval is usable only once known.
    let request = UniverseRequest::new(days()[2]).with_known_at(days()[0]);
    let answer = universe(&c, &request).unwrap();
    assert_eq!(
        answer.temporal.dataset("risk_warnings").unwrap().rows,
        1,
        "the one stored risk interval is counted"
    );
}

/// AC-M2-8: a missing bar is a data gap, never a suspension.
#[test]
fn a_bar_gap_is_unknown_data_gap_not_suspended() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();
    let report = tradability(&c, "sz.000631", Temporal::effective_only(days[3])).unwrap();
    assert_eq!(
        report.state,
        Tradability::Unknown(TradabilityReason::DataGap)
    );
    assert_eq!(report.reason_code(), "data_gap");
    assert!(!report.has_bar);
    let suspension = report.suspension.expect("the gap row is the evidence");
    assert_eq!(
        suspension.certainty,
        crate::security_master::SuspensionCertainty::UnresolvedGap
    );
    assert_eq!(
        suspension.reason,
        crate::security_master::SuspensionReason::MissingBarNoEvent
    );
    assert_eq!(suspension.missing_days, 8);
    assert_eq!(suspension.start_day, days[1]);
    assert_eq!(suspension.end_day, days[8]);

    // The bar day is tradable; the first gap day and the last gap day are not.
    let report = tradability(&c, "sz.000631", Temporal::effective_only(days[0])).unwrap();
    assert_eq!(report.state, Tradability::Tradable);
    for index in [1usize, 8] {
        let report = tradability(&c, "sz.000631", Temporal::effective_only(days[index])).unwrap();
        assert_eq!(
            report.state,
            Tradability::Unknown(TradabilityReason::DataGap)
        );
    }

    // A confirmed suspension is the only path to `false` (§7.4 step 3).
    let report = tradability(&c, "sz.000004", Temporal::effective_only(days[3])).unwrap();
    assert_eq!(
        report.state,
        Tradability::NotTradable(TradabilityReason::Suspended)
    );
    assert_eq!(report.reason_code(), "suspended");

    // And a known_at filter that excludes the confirmed announcement turns the
    // answer into `unknown` (no bar), never into `suspended`.
    let report = tradability(&c, "sz.000004", Temporal::known_by(days[3], days[1])).unwrap();
    assert_eq!(report.state, Tradability::Unknown(TradabilityReason::NoBar));
    assert_eq!(report.temporal.dataset("suspensions").unwrap().rows, 1);
    assert_eq!(
        report
            .temporal
            .dataset("suspensions")
            .unwrap()
            .excluded_known_at_rows,
        1
    );
}

#[test]
fn tradability_reports_the_calendar_and_listing_states() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();
    // A Saturday: not in the derived calendar.
    let saturday = day(2006, 10, 14);
    let report = tradability(&c, "sz.000001", Temporal::effective_only(saturday)).unwrap();
    assert_eq!(
        report.state,
        Tradability::NotTradable(TradabilityReason::NonTradingDay)
    );
    // Before the listing date.
    let report = tradability(&c, "sz.000002", Temporal::effective_only(days[0])).unwrap();
    assert_eq!(
        report.state,
        Tradability::NotTradable(TradabilityReason::NotListed)
    );
    // After the delisting date.
    let report = tradability(&c, "sz.000003", Temporal::effective_only(days[DELIST_DAY])).unwrap();
    assert_eq!(
        report.state,
        Tradability::NotTradable(TradabilityReason::Delisted)
    );
    // An entity that is not in the main table at all.
    let report = tradability(&c, "sz.999999", Temporal::effective_only(days[0])).unwrap();
    assert_eq!(report.reason_code(), "not_listed");
}

#[test]
fn a_missing_calendar_is_unknown_not_a_non_trading_day() {
    let fixture = Fixture::without_calendar();
    let c = fixture.read();
    let report = tradability(&c, "sz.000001", Temporal::effective_only(days()[0])).unwrap();
    assert_eq!(
        report.state,
        Tradability::Unknown(TradabilityReason::CalendarMissing)
    );
}

/// AC-M2-6: the A-share total is `unknown`; the company total is reachable but
/// labelled as such.
#[test]
fn a_plus_h_a_share_total_is_unknown_and_company_total_is_labelled() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let as_of = days()[0];
    let temporal = Temporal::effective_only(as_of);
    let options = MarketCapOptions::default();

    let float_a =
        super::market_cap::market_cap(&c, "sh.600028", temporal, ShareScope::FloatA, &options)
            .unwrap();
    assert_eq!(float_a.state, MarketCapState::Known);
    assert_eq!(float_a.shares, Some(1_000_000_000));
    // 10.00 元 × 1.0e9 shares = 1.0e12 分.
    assert_eq!(float_a.value_units, Some(1_000_000_000_000));
    assert_eq!(float_a.algorithm_version, "market-cap-v1");

    let company = super::market_cap::market_cap(
        &c,
        "sh.600028",
        temporal,
        ShareScope::CompanyTotal,
        &options,
    )
    .unwrap();
    assert_eq!(company.state, MarketCapState::Known);
    assert_eq!(company.shares, Some(1_200_000_000));
    assert_eq!(company.value_units, Some(1_200_000_000_000));

    let a_share =
        super::market_cap::market_cap(&c, "sh.600028", temporal, ShareScope::AShareTotal, &options)
            .unwrap();
    assert_eq!(a_share.state, MarketCapState::Unknown);
    assert_eq!(
        a_share.reason,
        Some(MarketCapReason::AShareTotalNotAdmitted)
    );
    assert_eq!(a_share.value_units, None);
    assert_eq!(a_share.known_at_coverage, KnownAtCoverage::None);
}

#[test]
fn market_cap_without_a_bar_or_a_share_event_is_unknown() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let options = MarketCapOptions::default();
    let days = days();
    // 000001 has no stored capital event at all.
    let answer = super::market_cap::market_cap(
        &c,
        "sz.000001",
        Temporal::effective_only(days[0]),
        ShareScope::FloatA,
        &options,
    )
    .unwrap();
    assert_eq!(answer.state, MarketCapState::Unknown);
    assert_eq!(answer.reason, Some(MarketCapReason::NoShareEvent));

    // A known_at filter excludes every capital event (gbbq has no announcement
    // day), so the answer is unknown and the exclusion is counted.
    let answer = super::market_cap::market_cap(
        &c,
        "sh.600028",
        Temporal::known_by(days[0], days[0]),
        ShareScope::FloatA,
        &options,
    )
    .unwrap();
    assert_eq!(answer.state, MarketCapState::Unknown);
    assert_eq!(answer.reason, Some(MarketCapReason::NoShareEvent));
    assert_eq!(answer.temporal.unknown_known_at_rows(), 1);
    assert_eq!(
        answer.temporal.datasets_without_known_at(),
        vec!["capital_events"]
    );
}

#[test]
fn the_end_reconciliation_gate_flags_and_can_exclude_a_float_history() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let as_of = days()[0];
    let temporal = Temporal::effective_only(as_of);
    // 1.2e9 vs 1.0e9 is 20% away: G2 flags the float history.
    let options = MarketCapOptions {
        shares: ShareOptions {
            current_float_reference: Some(1_200_000_000),
            ..ShareOptions::default()
        },
        exclude_low_confidence: false,
    };
    let answer =
        super::market_cap::market_cap(&c, "sh.600028", temporal, ShareScope::FloatA, &options)
            .unwrap();
    assert!(answer.low_confidence);
    assert!(matches!(
        answer.float_reconciliation,
        Reconciliation::LowConfidence { diff_bp: 1666, .. }
    ));
    assert_eq!(
        answer.state,
        MarketCapState::Known,
        "the flag is not a veto"
    );

    let options = MarketCapOptions {
        exclude_low_confidence: true,
        ..options
    };
    let answer =
        super::market_cap::market_cap(&c, "sh.600028", temporal, ShareScope::FloatA, &options)
            .unwrap();
    assert_eq!(answer.state, MarketCapState::Unknown);
    assert_eq!(answer.reason, Some(MarketCapReason::LowConfidence));

    // Within 1% is fine.
    let options = MarketCapOptions {
        shares: ShareOptions {
            current_float_reference: Some(1_005_000_000),
            ..ShareOptions::default()
        },
        exclude_low_confidence: true,
    };
    let answer =
        super::market_cap::market_cap(&c, "sh.600028", temporal, ShareScope::FloatA, &options)
            .unwrap();
    assert!(!answer.low_confidence);
    assert_eq!(answer.state, MarketCapState::Known);
}

fn event(
    ordinal: i64,
    day: NaiveDate,
    float_before: f32,
    total_before: f32,
    float_after: f32,
    total_after: f32,
) -> CapitalEvent {
    CapitalEvent::from_raw(
        ordinal,
        day,
        9,
        [
            f32::to_bits(float_before),
            f32::to_bits(total_before),
            f32::to_bits(float_after),
            f32::to_bits(total_after),
        ],
    )
    .unwrap()
}

/// The §5.2 state function plus the two D-50 gates, without a database.
#[test]
fn share_state_skips_suspicious_events_and_stops_at_a_chain_break() {
    let days = days();
    let options = ShareOptions::default();
    let events = vec![
        event(0, days[0], 0.0, 0.0, 100_000.0, 120_000.0),
        event(1, days[2], 100_000.0, 120_000.0, 130_000.0, 150_000.0),
    ];
    let state = shares_at(&events, days[1], &options);
    assert_eq!(state.float_shares, Some(1_000_000_000));
    assert_eq!(state.total_shares, Some(1_200_000_000));
    assert_eq!(state.event_day, Some(days[0]));
    let state = shares_at(&events, days[3], &options);
    assert_eq!(state.float_shares, Some(1_300_000_000));
    assert_eq!(state.total_shares, Some(1_500_000_000));

    // A suspicious event (the total jumps by more than 50% in one record) does
    // not become state unless explicitly relaxed.
    let suspicious = event(2, days[4], 130_000.0, 150_000.0, 130_000.0, 500_000.0);
    assert!(
        suspicious.suspicious,
        "the fixture event must trip the gate"
    );
    let mut with_suspicious = events.clone();
    with_suspicious.push(suspicious);
    let state = shares_at(&with_suspicious, days[5], &options);
    assert_eq!(state.float_shares, Some(1_300_000_000));
    assert_eq!(state.skipped_suspicious, 1);
    let relaxed = ShareOptions {
        allow_suspicious: true,
        ..ShareOptions::default()
    };
    let state = shares_at(&with_suspicious, days[5], &relaxed);
    assert_eq!(state.total_shares, Some(5_000_000_000));

    // A chain break (the next "before" does not match the previous "after")
    // makes the history from the break on unknown; nothing is bridged.
    let mut broken = events.clone();
    // Not suspicious on its own (+33% float, +20% total): the only defect is
    // that its "before" does not match the previous "after".
    broken[1] = event(1, days[2], 150_000.0, 150_000.0, 200_000.0, 180_000.0);
    let state = shares_at(&broken, days[1], &options);
    assert_eq!(state.float_shares, Some(1_000_000_000));
    let state = shares_at(&broken, days[2], &options);
    assert_eq!(state.reason, Some(ShareReason::ChainBreak));
    assert_eq!(state.chain_break_day, Some(days[2]));
    assert_eq!(state.float_shares, None);

    // No event at all.
    let state = shares_at(&[], days[0], &options);
    assert_eq!(state.reason, Some(ShareReason::NoEvent));
}

#[test]
fn a_member_state_lookup_returns_the_interval_covering_the_day() {
    let fixture = Fixture::new();
    let c = fixture.read();
    let days = days();
    let before = member_state(&c, "sh.600018", &UniverseRequest::new(days[REUSE_DAY - 1]))
        .unwrap()
        .unwrap();
    assert_eq!(before.effective_from, day(2000, 7, 19));
    assert_eq!(before.issuer_resolution, IssuerResolution::CodeDate);
    let after = member_state(&c, "sh.600018", &UniverseRequest::new(days[REUSE_DAY]))
        .unwrap()
        .unwrap();
    assert_eq!(after.effective_from, days[REUSE_DAY]);
    assert_eq!(after.issuer_resolution, IssuerResolution::Ambiguous);
    assert!(matches!(after.membership, Membership::In(_)));

    // A code that is not in the main table.
    assert!(
        member_state(&c, "sz.999999", &UniverseRequest::new(days[0]))
            .unwrap()
            .is_none()
    );
    // The index row is never an A-share member.
    let index = member_state(&c, "sh.000300", &UniverseRequest::new(days[0]))
        .unwrap()
        .unwrap();
    assert_eq!(index.membership.reason(), Some(NotMemberReason::NotAShare));
}
