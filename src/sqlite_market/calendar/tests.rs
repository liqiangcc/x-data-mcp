//! Trading-calendar storage tests (issue-383 §3.8/§5.2, AC-M1-4/AC-M1-5): the
//! whole-table replace, the version registration and its evidence, the
//! coverage self-check measured against the stored 不复权 series, and the
//! threshold gate.
//!
//! The raw series the coverage check reads are written through the production
//! daily commit path (`commit_item_tx` with its raw half), so the check runs
//! against rows shaped exactly as PR-2 writes them rather than hand-inserted
//! fixtures.

use std::collections::BTreeSet;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;
use serde_json::json;

use super::{CalendarCoverage, CalendarWrite, read_calendar, read_days};
use crate::compact_market::{self, EvidenceKind, PRICE_SCALE_QFQ, SourceBinding, VersionEvidence};
use crate::market_data::{
    Adjustment, AdjustmentEventSet, AdjustmentEvidence, Bar, CalendarBatch, CalendarInputDates,
    CoverageStatus, EvidenceValue, Period, RawBatch, UnitCurrencyEvidence,
};
use crate::market_storage::{MarketRepository, Scope};
use crate::sqlite_market::compact_repo::SqliteCompactMarket;
use crate::sqlite_market::derived::DerivedVersionRow;
use crate::sqlite_market::evidence_store;
use crate::sqlite_market::series_commit::{self, Commit, SeriesChange};
use crate::sqlite_tasks::SqliteTasks;
use crate::task_control::{self, CommandKey, ItemSpec, TaskRepository};
use crate::trading_calendar::{TradingCalendar, TradingDay};
use rust_decimal::Decimal;

const RUN: &str = "run";
const ITEM: &str = "item-1";

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn repo(dir: &tempfile::TempDir) -> (std::path::PathBuf, SqliteCompactMarket) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("market.db");
    SqliteTasks::new(&path).initialize_compact().unwrap();
    // The lease gate requires a private runtime path, exactly as the other
    // compact-store fixtures set it up.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = SqliteCompactMarket::open(&path).unwrap();
    (path, repo)
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-10-26T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn bar(day: NaiveDate) -> Bar {
    Bar {
        date: day,
        open: Decimal::new(1000, 2),
        high: Decimal::new(1010, 2),
        low: Decimal::new(990, 2),
        close: Decimal::new(1005, 2),
        volume: Some(Decimal::from(100)),
        amount: Some(Decimal::new(100_500, 2)),
        r7_raw: Some(7),
    }
}

/// Accept + claim + start a one-item daily task for `entity` — the same task
/// machinery the production runner drives, so the raw commit's gates are real.
fn running_task(repo: &SqliteCompactMarket, request: &str, entity: &str) -> (String, u64) {
    let scope = task_control::legacy::scope_id(entity, "day");
    let work = task_control::FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": entity, "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = task_control::Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept(request, &command, &work, RUN, None).unwrap();
    let task_id = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    let revision = repo.task(&task_id).unwrap().task.state_revision;
    let revision = repo
        .transition(&task_id, revision, RUN, task_control::Change::Claim)
        .unwrap()
        .task
        .state_revision;
    let revision = repo
        .transition(
            &task_id,
            revision,
            RUN,
            task_control::Change::StartItem(ITEM.into()),
        )
        .unwrap()
        .task
        .state_revision;
    (task_id, revision)
}

/// One security's published version evidence (the raw half re-reads it and
/// only swaps the adjustment 口径).
fn version_evidence() -> VersionEvidence {
    VersionEvidence {
        source: "official".into(),
        coverage_reason: "fixture".into(),
        unit_currency: UnitCurrencyEvidence {
            price_currency: EvidenceValue::confirmed("CNY", "bound"),
            price_unit: EvidenceValue::confirmed("share", "bound"),
            volume_unit: EvidenceValue::confirmed("share", "bound"),
            amount_currency: EvidenceValue::confirmed("CNY", "bound"),
            amount_unit: EvidenceValue::confirmed("yuan", "bound"),
        },
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

/// Commit one security's daily pair (不复权 + 前复权) so `series_bars` holds the
/// 不复权 rows the coverage check counts.
fn commit_raw_series(repo: &SqliteCompactMarket, request: &str, entity: &str, days: &[NaiveDate]) {
    let bars: Vec<Bar> = days.iter().copied().map(bar).collect();
    let (task_id, revision) = running_task(repo, request, entity);
    let scope = Scope {
        symbol: entity.into(),
        period: Period::Day,
    };
    let occupancy_key = scope.occupancy_key();
    repo.tx(|c| {
        let version_ev = evidence_store::put_evidence(
            c,
            EvidenceKind::Version,
            &serde_json::to_value(version_evidence()).unwrap(),
        )?;
        let generation = evidence_store::put_evidence(
            c,
            EvidenceKind::Generation,
            &json!({"generation_id": "gen-1"}),
        )?;
        let binding = evidence_store::put_source_binding(
            c,
            &SourceBinding {
                generation_id: "gen-1".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )?;
        let continuity = series_commit::put_continuity_evidence(c, &json!({"mode": "first"}))?;
        let forward = series_commit::to_compact_bars(&bars, Period::Day, PRICE_SCALE_QFQ)?;
        let raw = RawBatch::new(
            bars.clone(),
            AdjustmentEvidence::confirmed("raw", "fixture"),
            AdjustmentEventSet::empty(entity),
        );
        series_commit::commit_item_tx(
            c,
            &Commit {
                result_id: &format!("{request}-forward"),
                task_id: &task_id,
                item_id: ITEM,
                run_id: RUN,
                occupancy_key: &occupancy_key,
                expected_task_revision: revision,
                entity,
                period: Period::Day,
                adjustment: Adjustment::Forward,
                base_version_id: None,
                change: SeriesChange::FirstPublish { bars: &forward },
                range_start_day: 0,
                range_end_day: forward.len() as i64 - 1,
                algorithm_version: series_commit::COLLECTION_ALGO,
                window_digest: None,
                source_binding_digest: Some(binding),
                evidence_digest: Some(version_ev),
                coverage_status: Some(CoverageStatus::Complete),
                metadata_changed: false,
                event_evidence_digest: continuity,
                published_at: Some(ts()),
                staged_version_id: None,
                defer_task_row: false,
            },
            Some(&raw),
        )
        .map(|_| ())
    })
    .unwrap();
}

/// The adapter's facts for the two registered inputs.
fn calendar_batch(sh: &[NaiveDate], sz: &[NaiveDate]) -> CalendarBatch {
    CalendarBatch {
        inputs: vec![
            CalendarInputDates {
                entity: "sh.000001".into(),
                member: "sh\\lday\\sh000001.day".into(),
                day_member_sha256: "aa".repeat(32),
                days: sh.to_vec(),
            },
            CalendarInputDates {
                entity: "sz.399001".into(),
                member: "sz\\lday\\sz399001.day".into(),
                day_member_sha256: "bb".repeat(32),
                days: sz.to_vec(),
            },
        ],
        security_event_sha256: crate::hash::sha256(&0u64.to_le_bytes()),
    }
}

/// The generation manifest digest the write binds (64 hex characters).
const MANIFEST_SHA: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

/// Commit one calendar through the production write path.
fn commit(
    repo: &SqliteCompactMarket,
    batch: &CalendarBatch,
    coverage: &CalendarCoverage,
) -> Result<DerivedVersionRow, crate::market_storage::Error> {
    let generation_evidence = json!({"generation_id": "gen-1", "manifest_sha256": MANIFEST_SHA});
    repo.commit_calendar(&CalendarWrite {
        batch,
        coverage,
        task_id: "task-calendar",
        item_id: "derived:calendar",
        generation_id: "gen-1",
        generation_manifest_sha256: MANIFEST_SHA,
        generation_evidence: &generation_evidence,
    })
}

#[test]
fn commit_writes_the_rows_the_registration_and_the_evidence() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let sh = [day(1991, 4, 5), day(1991, 4, 8)];
    let sz = [day(1991, 4, 6), day(1991, 4, 8)];
    let batch = calendar_batch(&sh, &sz);
    let calendar = TradingCalendar::union(&batch).unwrap();
    // No 不复权 series yet: the check refuses to certify an unconfirmed
    // calendar (fail closed), so the commit cannot run either.
    let empty = repo.measure_calendar_coverage(&calendar).unwrap();
    assert_eq!(empty.total_bars, 0);
    assert!(
        commit(&repo, &batch, &empty)
            .unwrap_err()
            .to_string()
            .contains("no 不复权 bar")
    );
    commit_raw_series(
        &repo,
        "req-a",
        "sh.600006",
        &[day(1991, 4, 6), day(1991, 4, 8)],
    );
    let coverage = repo.measure_calendar_coverage(&calendar).unwrap();
    assert_eq!((coverage.total_bars, coverage.outside_bars), (2, 0));
    let written = commit(&repo, &batch, &coverage).unwrap();

    // §3.8: one row per date, mask OR-ed across inputs.
    let c = Connection::open(&path).unwrap();
    let stored = read_days(&c).unwrap();
    assert_eq!(
        stored,
        vec![
            TradingDay {
                day: day(1991, 4, 5),
                sources: 1
            },
            TradingDay {
                day: day(1991, 4, 6),
                sources: 2
            },
            TradingDay {
                day: day(1991, 4, 8),
                sources: 3
            },
        ]
    );
    // The 1991 Saturday Shenzhen session is a trading day even though the
    // Shanghai index has no bar: the union, not a weekday rule (§5.2).
    assert_eq!(stored[1].sources, 1 << 1);

    // §3.10: the registration is the digest's hex, the digest format and the
    // algorithm label the design names, and the span is the input span.
    assert_eq!(written.dataset_kind, "trading_calendar");
    assert_eq!(written.dataset_key, "exchange_union");
    assert_eq!(written.version_id, calendar.digest_hex());
    assert_eq!(written.digest_format, "trading-calendar-v1");
    assert_eq!(written.algorithm_version, "cal-union-v1");
    assert_eq!(written.record_count, 3);
    assert_eq!(
        written.first_day,
        compact_market::epoch_day(day(1991, 4, 5))
    );
    assert_eq!(written.last_day, compact_market::epoch_day(day(1991, 4, 8)));
    // AC-M1-5: the input list and the algorithm version are queryable.
    let (kind, canonical) = evidence_store::read_evidence(&c, &written.evidence_digest).unwrap();
    assert_eq!(kind, EvidenceKind::Derived);
    let evidence: serde_json::Value = serde_json::from_str(&canonical).unwrap();
    assert_eq!(evidence["algorithm_version"], "cal-union-v1");
    assert_eq!(evidence["day_count"], 3);
    assert_eq!(evidence["inputs"][0]["entity"], "sh.000001");
    assert_eq!(evidence["inputs"][0]["day_count"], 2);
    assert_eq!(evidence["inputs"][1]["entity"], "sz.399001");
    assert_eq!(evidence["inputs"][1]["sources_bit"], 2);
    assert_eq!(evidence["inputs"][1]["day_member_sha256"], "bb".repeat(32));
    assert_eq!(evidence["coverage"]["total_bars"], 2);
    assert_eq!(evidence["coverage"]["outside_bars"], 0);
    assert_eq!(evidence["coverage"]["limit"], "1/10000 (0.01%)");
    assert_eq!(evidence["coverage"]["days_truncated"], false);
    // The one-way semantics and the completeness limit travel with the data.
    assert!(
        evidence["semantics"]
            .as_str()
            .unwrap()
            .contains("does NOT mean a security could be traded")
    );
    assert!(
        evidence["limitations"][0]
            .as_str()
            .unwrap()
            .contains("no official calendar file")
    );
    assert!(
        evidence["beyond_last_day"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    // The calendar is a normal derived dataset: its source binding exists and
    // points at the generation.
    let binding = evidence_store::read_source_binding(&c, &written.source_binding_digest).unwrap();
    assert_eq!(binding.generation_id, "gen-1");
    // A calendar binds the empty corporate-action set, the same digest an
    // index series binds (sha256 over the zero excluded-count word).
    assert_eq!(
        compact_market::digest_hex(&binding.security_event_sha256),
        crate::hash::sha256(&0u64.to_le_bytes())
    );
    assert!(read_calendar(&c).unwrap().is_some());
}

#[test]
fn a_second_commit_replaces_the_whole_table() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    commit_raw_series(&repo, "req-a", "sh.600006", &[day(2026, 10, 22)]);
    let first_batch = calendar_batch(
        &[day(2026, 10, 22), day(2026, 10, 23)],
        &[day(2026, 10, 22)],
    );
    let first_coverage = repo
        .measure_calendar_coverage(&TradingCalendar::union(&first_batch).unwrap())
        .unwrap();
    let first = commit(&repo, &first_batch, &first_coverage).unwrap();
    // The next derivation drops 10-23 (the inputs no longer carry it) and adds
    // 10-26; the stored table must be exactly the new derivation.
    let second_batch = calendar_batch(
        &[day(2026, 10, 22)],
        &[day(2026, 10, 22), day(2026, 10, 26)],
    );
    let second_coverage = repo
        .measure_calendar_coverage(&TradingCalendar::union(&second_batch).unwrap())
        .unwrap();
    let second = commit(&repo, &second_batch, &second_coverage).unwrap();
    let c = Connection::open(&path).unwrap();
    assert_eq!(
        read_days(&c).unwrap(),
        vec![
            TradingDay {
                day: day(2026, 10, 22),
                sources: 3
            },
            TradingDay {
                day: day(2026, 10, 26),
                sources: 2
            },
        ]
    );
    assert_ne!(first.version_id, second.version_id);
    // One registration row, not one per derivation (§3.7 primary key).
    let rows: i64 = c
        .query_row("SELECT count(*) FROM derived_dataset_versions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 1);
    // Re-deriving the same content is the same version (§3.10).
    let again = commit(&repo, &second_batch, &second_coverage).unwrap();
    assert_eq!(again.version_id, second.version_id);
}

/// AC-M1-4: bars outside the calendar are counted, listed and gated at 0.01%.
#[test]
fn bars_outside_the_calendar_are_listed_and_gate_the_commit() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let outside = day(1991, 4, 7);
    commit_raw_series(&repo, "req-a", "sh.600006", &[day(1991, 4, 6), outside]);
    commit_raw_series(&repo, "req-b", "sz.000002", &[outside, day(1991, 4, 8)]);
    // The calendar covers the two dates the inputs printed, so three of the
    // four bars fall outside it.
    let batch = calendar_batch(&[day(1991, 4, 6)], &[day(1991, 4, 8)]);
    let calendar = TradingCalendar::union(&batch).unwrap();
    let coverage = repo.measure_calendar_coverage(&calendar).unwrap();
    assert_eq!(coverage.total_bars, 4);
    assert_eq!(coverage.outside_bars, 2);
    assert_eq!(coverage.outside_days, vec![(outside, 2)]);
    // The 清单 names the bars, not just the count.
    assert_eq!(
        coverage.outside_samples,
        vec![
            ("sh.600006".to_string(), outside),
            ("sz.000002".to_string(), outside)
        ]
    );
    assert!(coverage.ratio_text().contains("2/4"));
    let error = commit(&repo, &batch, &coverage).unwrap_err();
    assert!(
        error.to_string().contains("fall outside the calendar"),
        "{error}"
    );
    // The failed commit left no calendar and no registration behind.
    let c = Connection::open(&_path).unwrap();
    let counts: (i64, i64) = c
        .query_row(
            "SELECT (SELECT count(*) FROM trading_days),\
             (SELECT count(*) FROM derived_dataset_versions)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(counts, (0, 0));
}

/// The coverage figure belongs to exactly one calendar: a measurement taken
/// against another union is refused instead of being registered.
#[test]
fn coverage_from_another_calendar_is_refused() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    commit_raw_series(&repo, "req-a", "sh.600006", &[day(2026, 10, 22)]);
    let measured = calendar_batch(&[day(2026, 10, 22)], &[day(2026, 10, 22)]);
    let coverage = repo
        .measure_calendar_coverage(&TradingCalendar::union(&measured).unwrap())
        .unwrap();
    let other = calendar_batch(
        &[day(2026, 10, 22), day(2026, 10, 23)],
        &[day(2026, 10, 22)],
    );
    let error = commit(&repo, &other, &coverage).unwrap_err();
    assert!(error.to_string().contains("different calendar"), "{error}");
}

/// The coverage check only counts 不复权 security bars: an index series and a
/// 前复权 series are different data, and neither may decide whether the
/// calendar is complete.
#[test]
fn coverage_counts_raw_security_bars_only() {
    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    commit_raw_series(&repo, "req-a", "sh.600006", &[day(2026, 10, 22)]);
    let batch = calendar_batch(&[day(2026, 10, 22)], &[day(2026, 10, 22)]);
    let calendar = TradingCalendar::union(&batch).unwrap();
    let coverage = repo.measure_calendar_coverage(&calendar).unwrap();
    // One security, one day, both 口径 stored: the pair commit writes a 不复权
    // and a 前复权 row, and only the 不复权 row is checked.
    assert_eq!(coverage.total_bars, 1);
    assert_eq!(coverage.outside_bars, 0);
    let c = Connection::open(&_path).unwrap();
    let series: i64 = c
        .query_row("SELECT count(*) FROM market_series", [], |r| r.get(0))
        .unwrap();
    assert_eq!(series, 2);
}
