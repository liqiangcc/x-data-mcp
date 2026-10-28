use std::os::unix::fs::PermissionsExt;

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::json;

use super::*;
use crate::compact_market::{EvidenceKind, SourceBinding};
use crate::sqlite_tasks::SqliteTasks;
use crate::trading_rules::{
    BandSide, BasisStatus, ListingPhase, MinTickRule, PriceLimitRule, Rounding, RuleBoard,
    RuleMarket, StatusClass,
};

fn day(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn rule(id: &str, market: RuleMarket, band_bp: i64) -> PriceLimitRule {
    PriceLimitRule {
        rule_id: id.into(),
        market,
        board: RuleBoard::Main,
        status_class: StatusClass::Normal,
        listing_phase: ListingPhase::Seasoned,
        band_bp: Some(band_bp),
        band_side: BandSide::Both,
        rounding: Rounding::HalfUp,
        effective_from: day(2020, 1, 1),
        effective_to: None,
        basis_ref: "fixture".into(),
        basis_status: BasisStatus::Verified,
    }
}

fn tick(id: &str, market: RuleMarket) -> MinTickRule {
    MinTickRule {
        rule_id: id.into(),
        market,
        board: RuleBoard::Main,
        effective_from: day(2020, 1, 1),
        effective_to: None,
        tick_units: 1,
        price_scale: 2,
        basis_ref: "fixture".into(),
        basis_status: BasisStatus::Verified,
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    c: Connection,
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
        let c = Connection::open(&path).unwrap();
        c.pragma_update(None, "foreign_keys", true).unwrap();
        let generation = evidence_store::put_evidence(
            &c,
            EvidenceKind::Generation,
            &json!({"generation_id": "gen-1", "manifest_sha256": "fixture"}),
        )
        .unwrap();
        let binding = evidence_store::put_source_binding(
            &c,
            &SourceBinding {
                generation_id: "gen-1".into(),
                generation_manifest_sha256: [0x0b; 32],
                day_member_sha256: [0x0c; 32],
                security_event_sha256: [0x0e; 32],
                evidence_digest: generation,
            },
        )
        .unwrap();
        Self {
            _dir: dir,
            c,
            binding,
        }
    }
}

#[test]
fn the_rule_set_round_trips_and_registers_one_dataset_version() {
    let fixture = Fixture::new();
    let mut pending = rule("sz-pending", RuleMarket::Sz, 500);
    pending.status_class = StatusClass::St;
    pending.basis_status = BasisStatus::Pending;
    let book = RuleBook {
        price_limits: vec![
            rule("sh-normal", RuleMarket::Sh, 1_000),
            rule("sz-normal", RuleMarket::Sz, 1_000),
            pending,
        ],
        min_ticks: vec![
            tick("sh-tick", RuleMarket::Sh),
            tick("sz-tick", RuleMarket::Sz),
        ],
    };
    let row = replace_trading_rules(
        &fixture.c,
        &book,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap()
    .expect("non-empty registration");
    assert_eq!(row.record_count, 5);
    assert_eq!(row.algorithm_version, ALGORITHM_VERSION);
    assert_eq!(row.digest_format, DIGEST_FORMAT);
    assert_eq!(row.task_id, "task-rules");

    let read = read_trading_rules(&fixture.c).unwrap();
    assert_eq!(read, book);
    let price = read
        .price_limits
        .iter()
        .find(|rule| rule.rule_id == "sz-pending")
        .unwrap();
    assert_eq!(price.basis_status, BasisStatus::Pending);
}

#[test]
fn a_content_change_changes_the_version_id() {
    let fixture = Fixture::new();
    let first = RuleBook {
        price_limits: vec![rule("sz-normal", RuleMarket::Sz, 1_000)],
        min_ticks: vec![tick("sz-tick", RuleMarket::Sz)],
    };
    let first_row = replace_trading_rules(
        &fixture.c,
        &first,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap()
    .unwrap();

    let second = RuleBook {
        price_limits: vec![rule("sz-normal", RuleMarket::Sz, 2_000)],
        min_ticks: vec![tick("sz-tick", RuleMarket::Sz)],
    };
    let second_row = replace_trading_rules(
        &fixture.c,
        &second,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap()
    .unwrap();

    assert_ne!(first_row.version_id, second_row.version_id);
    assert_ne!(first_row.content_digest, second_row.content_digest);
    assert_eq!(read_trading_rules(&fixture.c).unwrap(), second);
}

#[test]
fn rewriting_removes_rows_that_are_no_longer_in_the_set() {
    let fixture = Fixture::new();
    let first = RuleBook {
        price_limits: vec![
            rule("sz-normal", RuleMarket::Sz, 1_000),
            rule("sh-normal", RuleMarket::Sh, 1_000),
        ],
        min_ticks: vec![
            tick("sz-tick", RuleMarket::Sz),
            tick("sh-tick", RuleMarket::Sh),
        ],
    };
    replace_trading_rules(
        &fixture.c,
        &first,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap();

    let second = RuleBook {
        price_limits: vec![rule("sz-normal", RuleMarket::Sz, 1_000)],
        min_ticks: vec![tick("sz-tick", RuleMarket::Sz)],
    };
    replace_trading_rules(
        &fixture.c,
        &second,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap();
    assert_eq!(read_trading_rules(&fixture.c).unwrap(), second);
}

#[test]
fn an_empty_set_removes_the_rows_and_the_registration() {
    let fixture = Fixture::new();
    let book = RuleBook {
        price_limits: vec![rule("sz-normal", RuleMarket::Sz, 1_000)],
        min_ticks: vec![tick("sz-tick", RuleMarket::Sz)],
    };
    replace_trading_rules(
        &fixture.c,
        &book,
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap();
    assert!(
        derived::read_version(&fixture.c, DATASET_KIND, trading_rules::DATASET_KEY)
            .unwrap()
            .is_some()
    );

    let row = replace_trading_rules(
        &fixture.c,
        &RuleBook::default(),
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap();
    assert!(row.is_none());
    assert!(
        read_trading_rules(&fixture.c)
            .unwrap()
            .price_limits
            .is_empty()
    );
    assert!(read_trading_rules(&fixture.c).unwrap().min_ticks.is_empty());
    assert!(
        derived::read_version(&fixture.c, DATASET_KIND, trading_rules::DATASET_KEY)
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_invalid_set_is_rejected_before_any_write() {
    let fixture = Fixture::new();
    let mut bad = rule("bad", RuleMarket::Sz, 1_000);
    bad.band_bp = None;
    let error = replace_trading_rules(
        &fixture.c,
        &RuleBook {
            price_limits: vec![bad],
            min_ticks: vec![tick("sz-tick", RuleMarket::Sz)],
        },
        fixture.binding,
        "task-rules",
        "derived:price_limit_rules",
    )
    .unwrap_err();
    assert!(matches!(error, Error::Integrity(_)));
    assert!(
        read_trading_rules(&fixture.c)
            .unwrap()
            .price_limits
            .is_empty()
    );
}

fn commit(c: &Connection, book: &RuleBook, task_id: &str) -> Result<TradingRulesCommit, Error> {
    let evidence = json!({"generation_id": "gen-1", "manifest_sha256": "fixture"});
    commit_trading_rules(
        c,
        &TradingRulesWrite {
            book,
            task_id,
            item_id: "derived:trading_rules",
            generation_id: "gen-1",
            generation_manifest_sha256: &"0f".repeat(32),
            member_sha256: &"0c".repeat(32),
            security_event_sha256: &"0e".repeat(32),
            generation_evidence: &evidence,
        },
    )
}

fn small_book() -> RuleBook {
    RuleBook {
        price_limits: vec![
            rule("sz-normal", RuleMarket::Sz, 1_000),
            rule("sh-normal", RuleMarket::Sh, 1_000),
        ],
        min_ticks: vec![
            tick("sz-tick", RuleMarket::Sz),
            tick("sh-tick", RuleMarket::Sh),
        ],
    }
}

#[test]
fn the_first_run_publishes_and_an_identical_run_is_no_change() {
    let fixture = Fixture::new();
    let book = small_book();
    let first = commit(&fixture.c, &book, "task-1").unwrap();
    assert!(!first.no_change);
    let published = first.registration.expect("a non-empty book registers");
    assert_eq!(published.task_id, "task-1");
    assert_eq!(published.record_count, 4);

    let second = commit(&fixture.c, &book, "task-2").unwrap();
    assert!(
        second.no_change,
        "identical content must not mint a version"
    );
    let kept = second.registration.expect("the stored version is returned");
    assert_eq!(kept.version_id, published.version_id);
    // The registration is left untouched: a no-change run does not re-stamp
    // the task/evidence that published the content.
    assert_eq!(kept.task_id, "task-1");
    assert_eq!(kept.updated_at, published.updated_at);
    assert_eq!(read_trading_rules(&fixture.c).unwrap(), book);
}

#[test]
fn a_changed_rule_book_publishes_a_new_version() {
    let fixture = Fixture::new();
    let mut book = small_book();
    let first = commit(&fixture.c, &book, "task-1").unwrap();
    book.price_limits[0].band_bp = Some(2_000);
    let second = commit(&fixture.c, &book, "task-2").unwrap();
    assert!(!second.no_change);
    assert_ne!(
        second.registration.unwrap().version_id,
        first.registration.unwrap().version_id
    );
    assert_eq!(read_trading_rules(&fixture.c).unwrap(), book);
}
