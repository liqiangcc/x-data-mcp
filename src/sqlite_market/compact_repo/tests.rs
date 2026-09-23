use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::json;

use super::*;
use crate::market_data::{
    AdjustmentEvidence, CoverageEvidence, EvidenceValue, UnitCurrencyEvidence,
};
use crate::market_storage::PublishIntent;
use crate::publish::ValidationSummary;
use crate::task_control::{CommandKey, ItemSpec};

fn tempdir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/dev/shm")
        .unwrap()
}

fn repo(dir: &tempfile::TempDir) -> (PathBuf, SqliteCompactMarket) {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("market.db");
    crate::sqlite_tasks::SqliteTasks::new(&path)
        .initialize19()
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let repo = SqliteCompactMarket::open(&path).unwrap();
    (path, repo)
}

fn ts() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-21T00:00:00+00:00")
        .unwrap()
        .with_timezone(&Utc)
}

fn date(day: i64) -> NaiveDate {
    compact_market::epoch_date(day).unwrap()
}

fn bar(day: i64, cents: i64) -> Bar {
    CompactBar {
        trade_day: day,
        open_cents: cents,
        high_cents: cents + 10,
        low_cents: cents - 10,
        close_cents: cents,
        volume: Some(100),
        amount: Some("1000".into()),
        r7_raw: Some(7),
    }
    .to_bar(Period::Day)
    .unwrap()
}

fn units() -> UnitCurrencyEvidence {
    UnitCurrencyEvidence {
        price_currency: EvidenceValue::confirmed("CNY", "bound"),
        price_unit: EvidenceValue::confirmed("share", "bound"),
        volume_unit: EvidenceValue::confirmed("share", "bound"),
        amount_currency: EvidenceValue::confirmed("CNY", "bound"),
        amount_unit: EvidenceValue::confirmed("yuan", "bound"),
    }
}

fn mk_version(version_id: &str, bars: Vec<Bar>) -> EffectiveVersion {
    let mut version = EffectiveVersion {
        version_id: version_id.into(),
        symbol: "sh.600006".into(),
        period: Period::Day,
        adjustment: Adjustment::Forward,
        source: "official".into(),
        records: bars,
        published_at: ts(),
        validation: ValidationSummary {
            record_count: 0,
            unique_record_count: 0,
            duplicate_count: 0,
        },
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        yearly_derivation: None,
    };
    let (records, validation) =
        crate::publish::validate_and_deduplicate(&mk_batch(&version)).unwrap();
    version.records = records;
    version.validation = validation;
    version
}

fn mk_batch(version: &EffectiveVersion) -> MarketDataBatch {
    MarketDataBatch {
        symbol: version.symbol.clone(),
        period: version.period,
        adjustment: Adjustment::Forward,
        bars: version.records.clone(),
        provenance: Provenance {
            source: version.source.clone(),
            source_period: "day".into(),
            source_adjustment: "qfq".into(),
            symbol: version.symbol.clone(),
            acquired_at: version.published_at,
            adapter_version: "effective-publish".into(),
            yearly_derivation: version.yearly_derivation.clone(),
            snapshot_inputs: None,
        },
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    }
}

fn source(generation: &str) -> Value {
    json!({
        "generation_id": generation,
        "generation_manifest_sha256": "ab".repeat(32),
        "day_member_sha256": "cd".repeat(32),
        "security_event_sha256": "ef".repeat(32),
        "generation_evidence": {
            "snapshot_inputs": [{
                "role": "daily",
                "member": "sh.600006.day",
                "archive_sha256": "12".repeat(32),
                "member_sha256": "cd".repeat(32)
            }]
        }
    })
}

fn envelope(version: &EffectiveVersion, source: Value, parent: Option<&str>) -> Value {
    let mut raw = serde_json::to_value(version).unwrap();
    raw["batch"] = serde_json::to_value(mk_batch(version)).unwrap();
    raw["compact_source"] = source;
    if let Some(parent) = parent {
        raw["compact_parent_version_id"] = json!(parent);
    }
    raw["integrity_sha256"] = json!(compat::legacy_digest(&raw).unwrap());
    raw
}

const ITEM: &str = "item-1";
const RUN: &str = "run";

fn accept_task(repo: &SqliteCompactMarket) -> String {
    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-1", &command, &work, RUN, None).unwrap();
    match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    }
}

fn claim(repo: &SqliteCompactMarket, task_id: &str) -> u64 {
    let revision = repo.task(task_id).unwrap().task.state_revision;
    repo.transition(task_id, revision, RUN, Change::Claim)
        .unwrap()
        .task
        .state_revision
}

fn start_item(repo: &SqliteCompactMarket, task_id: &str) -> u64 {
    let revision = claim(repo, task_id);
    repo.transition(task_id, revision, RUN, Change::StartItem(ITEM.into()))
        .unwrap()
        .task
        .state_revision
}

fn collect_command(
    task_id: &str,
    publication_id: &str,
    version_id: &str,
    revision: u64,
) -> Publication {
    Publication {
        publication_id: publication_id.into(),
        version_id: version_id.into(),
        task_id: task_id.into(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision,
        intent: PublishIntent::Daily {
            expected_head: None,
        },
    }
}

#[test]
fn collection_publish_end_to_end() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    assert_eq!(candidate.version.records.len(), 2);
    assert_eq!(
        candidate.raw()["compact_source"]["generation_id"],
        json!("gen-1")
    );
    // Deterministic synthesis: re-reading yields byte-identical raw.
    let again = repo.candidate("v1").unwrap();
    assert_eq!(candidate.raw(), again.raw());

    repo.mark_validated(&candidate).unwrap();
    let plan = PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    };
    let receipt = repo.commit(&plan).unwrap();
    assert_eq!(receipt.version_id, "v1");

    let fixed = repo.fixed("v1").unwrap();
    assert_eq!(fixed.version.records.len(), 2);
    // `published_at` is stamped at commit; every other fact is identical.
    assert_eq!(fixed.version.records, candidate.version.records);
    assert_eq!(
        fixed.raw()["compact_source"],
        candidate.raw()["compact_source"]
    );

    let scope = Scope::of(&fixed.version);
    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    // The page digest is the synthesized legacy digest of the fixed version
    // (published_at stamped at commit differs from the staged projection).
    assert!(crate::hash::is_sha256(&page.integrity_sha256));

    let versions = repo.versions(&scope).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version_id, "v1");
    assert!(repo.receipt("pub-1").unwrap().is_some());

    // Idempotent replay returns the same receipt without rewriting.
    let replay = repo.commit(&plan).unwrap();
    assert_eq!(replay, receipt);

    let binding = repo.task_item_binding(&task_id, ITEM).unwrap().unwrap();
    assert_eq!(binding.item.stage, task_control::Stage::Finished);
}

#[test]
fn append_update_promotes_head_and_preserves_base() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // Second task for the update item.
    let task_id2 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        let acceptance = repo.accept("req-2", &command, &work, RUN, None).unwrap();
        match acceptance.record.outcome {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision2 = start_item(&repo, &task_id2);

    let updated = mk_version("v2", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo.stage(&envelope(&updated, source("gen-2"), Some("v1")))
        .unwrap();
    let checked = repo.candidate("v2").unwrap();
    assert_eq!(checked.raw()["compact_parent_version_id"], json!("v1"));
    repo.mark_validated(&checked).unwrap();

    let receipt = repo
        .commit(&PublishPlan {
            command: Publication {
                publication_id: "upd-1".into(),
                version_id: "v2".into(),
                task_id: task_id2.clone(),
                item_id: ITEM.into(),
                run_id: RUN.into(),
                expected_revision: revision2,
                intent: PublishIntent::Daily {
                    expected_head: Some("v1".into()),
                },
            },
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), Some("v1".into()))],
            promote: None,
            base_digest: None,
        })
        .unwrap();
    assert_eq!(receipt.version_id, "v2");

    let head = repo
        .range(
            &Scope::of(&checked.version),
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(head.version.version_id, "v2");
    assert_eq!(head.rows.len(), 3);

    // The append stored ONLY the changed bar: delta segments never copy the
    // parent's tail chunk. 2 base bars + 1 new bar = 3 physical bar rows.
    let bar_rows: i64 = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(bar_rows, 3);

    // Base version remains immutable and independently readable.
    assert_eq!(repo.fixed("v1").unwrap().version.records.len(), 2);
    let receipt = repo.range_receipt("upd-1").unwrap().unwrap();
    assert_eq!(receipt.version_id, "v2");
    let item_receipt = repo.item_receipt(&task_id2, ITEM).unwrap().unwrap();
    assert_eq!(item_receipt.version_id, "v2");
}

#[test]
fn no_change_range_commit_writes_only_typed_result() {
    let dir = tempdir();
    let (path, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // New update task for the no-change range commit.
    let scope_id = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: "item-2".into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope_id.clone()]),
        }],
        scopes: BTreeSet::from([scope_id]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let acceptance = repo.accept("req-2", &command, &work, RUN, None).unwrap();
    let task2 = match acceptance.record.outcome {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    // The first task owns the scope until it finishes; the no-change commit
    // finishes it, so bind the update task after the commit would deadlock.
    // Instead the update task is accepted only after task-1's item finished.
    let claim2 = claim(&repo, &task2);
    let revision2 = repo
        .transition(&task2, claim2, RUN, Change::StartItem("item-2".into()))
        .unwrap()
        .task
        .state_revision;

    let counts = |repo: &SqliteCompactMarket| -> Vec<i64> {
        repo.read(|c| {
            [
                "data_versions",
                "bar_segments",
                "segment_bars",
                "market_update_results",
            ]
            .iter()
            .map(|table| {
                c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                    .map_err(storage)
            })
            .collect()
        })
        .unwrap()
    };
    let before = counts(&repo);

    let plan = RangePlan {
        publication: PublishPlan {
            command: Publication {
                publication_id: "upd-nochange".into(),
                version_id: "v1".into(),
                task_id: task2.clone(),
                item_id: "item-2".into(),
                run_id: RUN.into(),
                expected_revision: revision2,
                intent: PublishIntent::Daily {
                    expected_head: Some("v1".into()),
                },
            },
            record: base_checked.clone(),
            required_heads: vec![(Scope::of(&base_checked.version), Some("v1".into()))],
            promote: None,
            base_digest: None,
        },
        base: base_checked.clone(),
        no_change: true,
        audit: json!({
            "algorithm_version": "local-range-update-v1",
            "range": {"start": "2026-09-01", "end": "2026-09-21"},
            "evidence": {"same_qfq_basis_ref": "basis-1", "revision_ref": "rev-1"}
        }),
        audit_payloads: Vec::new(),
    };
    let receipt = repo.commit_range(&plan).unwrap();
    assert_eq!(receipt.version_id, "v1");

    let after = counts(&repo);
    // Only one typed market_update_results row is added — no bars, segments,
    // or versions are duplicated for a no-change update.
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(after[3], before[3] + 1);
    assert!(repo.range_receipt("upd-nochange").unwrap().is_some());
    assert!(path.exists());
}

#[test]
fn legacy_adapters_reject_schema19() {
    let dir = tempdir();
    let (path, _) = repo(&dir);
    let legacy =
        crate::sqlite_market::SqliteMarket::new(crate::sqlite_tasks::SqliteTasks::new(&path));
    assert!(legacy.task("missing").is_err());
    assert!(
        crate::sqlite_tasks::SqliteTasks::new(&path)
            .upgrade()
            .is_err()
    );
}

#[test]
fn backend_dispatch_and_compact_supply_surface() {
    use crate::data_supply::query::QueryPlan;
    use crate::data_supply::wire::{
        ContractVersion, Dataset, Exchange, Filter, Query, Security, Selection, VersionChoice,
        VersionsQuery,
    };
    use crate::data_supply::{Access, query::ReadRepository};
    use crate::sqlite_market::backend::MarketBackend;
    use crate::sqlite_supply::SqliteSupply;

    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    repo.mark_validated(&candidate).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // Schema-19 dispatch selects the compact backend. The service lease is
    // single-owner; `repo.tasks()` is coordinator-bound and keeps the lease
    // alive, so the dispatch probe uses a fresh handle after both drops.
    let tasks = repo.tasks();
    let path = tasks.path().to_path_buf();
    drop(repo);
    drop(tasks);
    let backend = MarketBackend::open(&SqliteTasks::new(&path)).unwrap();
    assert!(matches!(backend, MarketBackend::Compact(_)));
    assert!(backend.task(&task_id).is_ok());
    assert_eq!(backend.tasks().path(), path.as_path());

    // The compact read surface serves versions + query + verify.
    let supply = SqliteSupply {
        market: backend.clone(),
    };
    let access = Access::local();
    let entity = Security {
        exchange: Exchange::Sh,
        code: "600006".into(),
    };
    let infos = supply
        .versions(
            &VersionsQuery {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                selection: Selection::Single {
                    entity: entity.clone(),
                },
                page_size: 10,
                cursor: None,
            },
            &access,
        )
        .unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].version_ref.version_id, "v1");

    let plan = QueryPlan::new(
        Query {
            dataset: Dataset::Daily,
            contract_version: ContractVersion::V1,
            selection: Selection::Single { entity },
            filter: Filter::Daily {
                start_date: date(0),
                end_date: date(10),
            },
            version: VersionChoice::LatestPerEntity,
            page_size: 10,
            cursor: None,
        },
        &access,
    )
    .unwrap();
    let snapshot = supply.query(&plan, &access).unwrap();
    assert_eq!(snapshot.rows.len(), 2);

    // verify_versions round-trips the same snapshot digests.
    supply.verify(&[infos[0].version_ref.clone()]).unwrap();

    // Schema-19 task facts: progress inspection reads typed
    // `publication_results`/`market_update_results`, not the dropped
    // `yearly_execution_results`/`publication_batches` tables.
    use crate::data_supply::observe::ObservationRepository;
    let facts = supply.task(&task_id, &access).unwrap();
    assert_eq!(facts.request_id, "req-1");
    assert_eq!(facts.publications.len(), 1);
    assert!(facts.range_results.is_empty());
    assert!(facts.yearly_results.is_empty());

    // Stopped-write evidence adapters read the same compact result tables.
    use crate::task_verification::determinate::EvidenceRepository as _;
    use crate::task_verification::never_started::EvidenceRepository as _;
    assert!(
        backend
            .tasks()
            .never_started_facts(&task_id)
            .unwrap()
            .has_durable_effects
    );
    let determinate = backend.tasks().determinate_facts(&task_id).unwrap();
    assert!(determinate.has_durable_effects);
    assert!(determinate.committed_items.contains(ITEM));

    // The compact backend's task handle is coordinator-bound: legacy
    // SqliteTasks callers (task control, verification adapters) work on a
    // schema-19 database without touching check_schema's v18 gate.
    use crate::task_control::TaskRepository;
    let bound_tasks = backend.tasks();
    let accepted = bound_tasks
        .accept(
            "req-coordinator-bound",
            &crate::task_control::Command {
                key: crate::task_control::CommandKey {
                    operation: "collect".into(),
                    dataset: "market.daily".into(),
                    contract_version: "v1".into(),
                },
                intent: serde_json::json!({}),
            },
            &crate::task_control::FrozenWork {
                items: vec![crate::task_control::ItemSpec {
                    item_id: "i1".into(),
                    target: serde_json::json!({"symbol": "sh.600006"}),
                    progress_unit: "security_period".into(),
                    frozen_inputs: serde_json::json!({}),
                    scope_keys: std::collections::BTreeSet::from(["scope-1".to_string()]),
                }],
                scopes: std::collections::BTreeSet::from(["scope-1".to_string()]),
                shared_evidence: None,
            },
            "run-x",
            None,
        )
        .unwrap();
    let crate::task_control::RequestOutcome::Task(snapshot) = accepted.record.outcome else {
        panic!("expected task outcome");
    };
    assert_eq!(
        bound_tasks
            .read_task(&snapshot.task.task_id)
            .unwrap()
            .unwrap()
            .task
            .task_id,
        snapshot.task.task_id
    );
    assert!(bound_tasks.candidates("run-x").unwrap().len() == 1);
    assert!(bound_tasks.candidates("other-run").unwrap().is_empty());

    // A second fresh v18 database still selects the legacy backend.
    let legacy_dir = tempdir();
    let legacy_path = legacy_dir.path().join("legacy.db");
    let legacy_tasks = SqliteTasks::new(legacy_path);
    legacy_tasks.initialize().unwrap();
    assert!(matches!(
        MarketBackend::open(&legacy_tasks).unwrap(),
        MarketBackend::Legacy(_)
    ));
}

#[test]
fn catalog_roundtrip_on_compact_schema_uses_bound_tasks() {
    use crate::catalog::{
        CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord, SecurityType,
    };
    use crate::sqlite_catalog::SqliteCatalog;

    let dir = tempdir();
    let (_path, repo) = repo(&dir);
    // The worker builds the catalog executor from `market.tasks()`; on a
    // schema-19 database that handle is coordinator-bound, so catalog
    // reads/writes must run through it rather than opening side
    // connections that bypass the shared lease and writer.
    let catalog = SqliteCatalog::new(repo.tasks());
    let now = Utc::now();
    let snapshot = CatalogSnapshot {
        source: "fixture:catalog".into(),
        acquired_at: now,
        completeness: CompletenessEvidence::Complete { expected_count: 1 },
        records: vec![SecurityRecord {
            symbol: "600006".into(),
            name: "样例".into(),
            exchange: Exchange::Shanghai,
            security_type: SecurityType::AShare,
            source_security_id: "sh600006".into(),
            industry: IndustryInfo::Missing {
                reason: "fixture unknown".into(),
            },
            source: "fixture:catalog".into(),
            acquired_at: now,
        }],
    };
    catalog
        .publish_fixture("cat-v1", snapshot.clone(), None)
        .unwrap();
    let facts = catalog.selection_facts().unwrap();
    assert_eq!(facts.catalog_head.as_deref(), Some("cat-v1"));
    assert_eq!(catalog.fixed("cat-v1").unwrap().records.len(), 1);
    assert!(matches!(
        catalog.publish_fixture("cat-v2", snapshot, Some("stale")),
        Err(crate::market_storage::Error::Conflict(_))
    ));
    assert_eq!(
        catalog.selection_facts().unwrap().catalog_head.as_deref(),
        Some("cat-v1")
    );
}

#[test]
fn yearly_derive_commit_on_compact_schema() {
    use crate::market_update::tasks::Operation;
    use crate::task_control::{Contract, RequestOutcome};
    use crate::yearly_publication::{
        DerivationEvidence,
        runner::{YearlyExecution, YearlyRunner, YearlyVerifier},
        tasks::YearlyContract,
    };

    struct Stub;
    impl YearlyVerifier for Stub {
        fn verify(
            &self,
            base: &CheckedRecord,
            years: &BTreeSet<i32>,
            _annual: &crate::yearly::AnnualSeries,
            derived_at: DateTime<Utc>,
        ) -> Result<DerivationEvidence, crate::market_data::SourceError> {
            Ok(DerivationEvidence {
                base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
                years: years.clone(),
                as_of: derived_at
                    .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                    .date_naive(),
                coverage: CoverageEvidence::complete("stub re-aggregation verified"),
            })
        }
    }

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    // Publish a daily base covering 2025 and 2026 through the real path.
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let days = [
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
        NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
    ]
    .into_iter()
    .map(compact_market::epoch_day);
    let version = mk_version(
        "v1",
        days.enumerate()
            .map(|(i, d)| bar(d, 100 + i as i64))
            .collect(),
    );
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    repo.mark_validated(&candidate).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // Accept the yearly task through the real contract, then run the item.
    let contract = YearlyContract {
        operation: Operation::Update,
        repository: repo.clone(),
        today: || NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
    };
    let work = contract
        .resolve(&json!({"items":[{
            "symbol":"sh.600006",
            "base_daily_version_id":"v1",
            "years":[2025,2026],
            "intent":{"CurrentHead":{"expected_daily_head":"v1","expected_yearly_head":null}},
            "comparison_version_id":null
        }]}))
        .unwrap();
    let command = Command {
        key: contract.key(),
        intent: json!({}),
    };
    let acceptance = repo
        .accept("req-yearly", &command, &work, RUN, None)
        .unwrap();
    let RequestOutcome::Task(snapshot) = acceptance.record.outcome else {
        panic!("expected task outcome")
    };
    let yearly_task = snapshot.task.task_id.clone();
    let item_id = snapshot.items[0].spec.item_id.clone();

    let runner = YearlyRunner {
        repository: repo.clone(),
        run_id: RUN.into(),
        derived_at: ts(),
    };
    let outcome = runner.run_item(&yearly_task, &item_id, &Stub).unwrap();
    let YearlyExecution::Published(receipt) = outcome else {
        let snap = repo.task(&yearly_task).unwrap();
        panic!("yearly item did not publish: {:?}", snap.items[0].outcome)
    };

    let fixed = repo.fixed(&receipt.version_id).unwrap();
    assert_eq!(fixed.version.period, Period::Year);
    assert_eq!(fixed.version.records.len(), 2);
    assert_eq!(
        fixed
            .version
            .yearly_derivation
            .as_ref()
            .unwrap()
            .base_daily_version_id,
        "v1"
    );
    // A derived version inherits the published base's source binding.
    assert_eq!(
        fixed.raw()["compact_source"]["generation_id"],
        json!("gen-1")
    );
    let head = repo
        .versions(&Scope {
            symbol: "sh.600006".into(),
            period: Period::Year,
        })
        .unwrap();
    assert_eq!(head.len(), 1);
    assert_eq!(head[0].version_id, receipt.version_id);

    // Re-running the committed item replays the stored receipt.
    let YearlyExecution::Published(replay) =
        runner.run_item(&yearly_task, &item_id, &Stub).unwrap()
    else {
        panic!("yearly replay did not publish")
    };
    assert_eq!(replay, receipt);

    // The audit projection requires the stored result to link its daily
    // derivation base (schema-19 task-progress reads hit this path).
    let (audit, _) = repo
        .read(|c| {
            compact_results::read_yearly_audit(c, &receipt.publication_id).map(|o| o.unwrap())
        })
        .unwrap();
    assert_eq!(audit["base_daily_version_id"], json!("v1"));
}

#[test]
fn ops_range_commit_stages_delta_binds_leaf_and_replays() {
    use crate::market_update::publication::RangeRepository;

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    let task_id2 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        match repo
            .accept("req-2", &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision2 = start_item(&repo, &task_id2);
    let scope = Scope::of(&base_checked.version);
    let approved = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
    let command = Publication {
        publication_id: "upd-ops-1".into(),
        version_id: "v2".into(),
        task_id: task_id2.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision2,
        intent: PublishIntent::Daily {
            expected_head: Some("v1".into()),
        },
    };

    // Leaf mismatch against the approved merge fails closed before commit.
    let wrong = vec![bar(0, 100), bar(1, 110), bar(2, 999)];
    assert!(
        repo.commit_range_ops(&command, &scope, "v1", &[bar(2, 120)], &[], &wrong)
            .is_err()
    );

    // Correct ops commit: staged delta binds leaf == approved merge.
    let receipt = repo
        .commit_range_ops(&command, &scope, "v1", &[bar(2, 120)], &[], &approved)
        .unwrap()
        .expect("compact backend supports ops commit");
    assert_eq!(receipt.version_id, "v2");

    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v2");
    assert_eq!(page.rows.len(), 3);

    // The delta stored only the changed bar row.
    let bar_rows: i64 = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM segment_bars", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(bar_rows, 3);

    // Replay: item receipt is already committed; a fresh commit_range_ops on
    // a new item id would restage harmlessly, but the committed item's
    // receipt is authoritative.
    let replay = repo.item_receipt(&task_id2, ITEM).unwrap().unwrap();
    assert_eq!(replay, receipt);
}

/// S5G typed path: metadata-only base facts, windowed base reads,
/// O(ops) staged-delta binding, and the no-change publication — all
/// without materializing the base leaf.
#[test]
fn typed_update_facts_window_ops_and_no_change() {
    use crate::market_update::publication::RangeRepository;

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);

    let base = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let base_checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&base_checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: base_checked.clone(),
        required_heads: vec![(Scope::of(&base_checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // 1. Typed base facts: version row + bindings + evidence, no bars.
    let facts = RangeRepository::update_base_facts(&repo, "v1")
        .unwrap()
        .expect("compact backend serves typed base facts");
    assert_eq!(facts.version_id, "v1");
    assert_eq!(facts.symbol, "sh.600006");
    assert_eq!(facts.record_count, 3);
    assert_eq!(facts.first_day, date(0));
    assert_eq!(facts.last_day, date(2));
    assert_eq!(facts.generation_id, "gen-1");
    assert_eq!(facts.day_member_sha256, "cd".repeat(32));
    assert_eq!(facts.security_event_sha256, "ef".repeat(32));
    // The typed facts carry the compact integrity digest (the content
    // binding covering projection + source inputs), not the synthesized
    // envelope's legacy digest.
    let stored_integrity: String = repo
        .read(|c| {
            c.query_row(
                "SELECT lower(hex(integrity_digest)) FROM data_versions WHERE version_id='v1'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(facts.integrity_sha256, stored_integrity);
    assert_eq!(facts.coverage.status, CoverageStatus::Complete);
    assert_eq!(facts.unit_currency, units());

    // 2. Windowed base read: only the in-range leaf bars decode.
    let window = RangeRepository::base_bars_in_range(&repo, "v1", date(1), date(2))
        .unwrap()
        .expect("windowed read supported");
    assert_eq!(window, vec![bar(1, 110), bar(2, 120)]);
    let window = RangeRepository::base_bars_in_range(&repo, "v1", date(5), date(9))
        .unwrap()
        .expect("windowed read supported");
    assert!(window.is_empty());

    // 3. Typed ops commit: staged delta binds by ops read-back, not leaf.
    let task_id2 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        match repo
            .accept("req-2", &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision2 = start_item(&repo, &task_id2);
    let scope = Scope::of(&base_checked.version);
    let command = Publication {
        publication_id: "upd-typed-1".into(),
        version_id: "v2".into(),
        task_id: task_id2.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision2,
        intent: PublishIntent::Daily {
            expected_head: Some("v1".into()),
        },
    };
    let evidence = json!({
        "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32),
            "day_prefix_bytes": 96, "gbbq_events_sha256": "ef".repeat(32)},
        "same_qfq_basis_ref": "inspected snapshot manifest x + gbbq member y",
    });

    // A staged version whose ops differ from the request fails closed.
    repo.stage_ops_delta("sh.600006", "v1", "v-bad", &[bar(3, 999)], &[])
        .unwrap();
    let bad_command = Publication {
        version_id: "v-bad".into(),
        ..command.clone()
    };
    assert!(
        repo.commit_range_ops_typed(&bad_command, &scope, "v1", &[bar(3, 130)], &[], None)
            .is_err()
    );

    let receipt = repo
        .commit_range_ops_typed(&command, &scope, "v1", &[bar(3, 130)], &[], Some(&evidence))
        .unwrap()
        .expect("typed ops commit supported");
    assert_eq!(receipt.version_id, "v2");
    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v2");
    assert_eq!(page.rows.len(), 4);

    // Replay of the committed typed result returns the same receipt.
    let replay = repo
        .commit_range_ops_typed(&command, &scope, "v1", &[bar(3, 130)], &[], None)
        .unwrap()
        .unwrap();
    assert_eq!(replay, receipt);

    // 4. No-change publication on a fresh item reuses the base version.
    let task_id3 = {
        let scope = task_control::legacy::scope_id("sh.600006", "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        match repo
            .accept("req-3", &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let revision3 = start_item(&repo, &task_id3);
    let nc_command = Publication {
        publication_id: "upd-typed-nc".into(),
        version_id: "v2".into(),
        task_id: task_id3.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision3,
        intent: PublishIntent::Daily {
            expected_head: Some("v2".into()),
        },
    };
    let nc_receipt = repo
        .commit_range_no_change_typed(
            &nc_command,
            "v2",
            &crate::market_update::DateRange {
                start: date(2),
                end: date(3),
            },
            Some(&evidence),
        )
        .unwrap()
        .expect("typed no-change commit supported");
    assert_eq!(nc_receipt.version_id, "v2");
    // Head is unchanged and no new version row was created.
    let page = repo
        .range(
            &scope,
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v2");
    let version_rows: i64 = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM data_versions", [], |r| r.get(0))
                .map_err(storage)
        })
        .unwrap();
    assert_eq!(version_rows, 3); // v1, v-bad (orphan draft), v2

    // A no-change command that names a different result version is rejected.
    let bad_nc = Publication {
        version_id: "v-other".into(),
        ..nc_command.clone()
    };
    assert!(
        repo.commit_range_no_change_typed(
            &bad_nc,
            "v2",
            &crate::market_update::DateRange {
                start: date(2),
                end: date(3),
            },
            None
        )
        .is_err()
    );
}

fn collection_candidate_for(
    version: &EffectiveVersion,
    task_id: &str,
    run: &str,
) -> CollectionCandidate {
    CollectionCandidate {
        task_id: task_id.into(),
        item_id: ITEM.into(),
        publication_id: "pub-1".into(),
        version_id: version.version_id.clone(),
        run_id: run.into(),
        symbol: version.symbol.clone(),
        source: version.source.clone(),
        bars: version.records.clone(),
        coverage: CoverageEvidence::complete("full history verified"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
        published_at: version.published_at,
        generation_id: "gen-1".into(),
        generation_manifest_sha256: "ab".repeat(32),
        day_member_sha256: "cd".repeat(32),
        security_event_sha256: "ef".repeat(32),
        generation_evidence: json!({
            "snapshot_inputs": [{
                "role": "daily",
                "member": "sh.600006.day",
                "archive_sha256": "12".repeat(32),
                "member_sha256": "cd".repeat(32)
            }]
        }),
    }
}

#[test]
fn typed_collection_batch_is_byte_equal_to_envelope_path() {
    // Envelope path on repo1.
    let dir1 = tempdir();
    let (_, repo1) = repo(&dir1);
    let task_id = accept_task(&repo1);
    let revision = start_item(&repo1, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo1
        .stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let checked = repo1.candidate("v1").unwrap();
    repo1.mark_validated(&checked).unwrap();
    repo1
        .commit(&PublishPlan {
            command: collect_command(&task_id, "pub-1", "v1", revision),
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();

    // Typed batch path on repo2 — same task inputs → same task_id.
    let dir2 = tempdir();
    let (_, repo2) = repo(&dir2);
    let task_id2 = accept_task(&repo2);
    let _ = (task_id, task_id2.clone());
    let results = repo2
        .commit_collection_batch(
            &task_id2,
            &[collection_candidate_for(&version, &task_id2, RUN)],
        )
        .unwrap();
    let receipt = results.into_iter().next().unwrap().unwrap();
    assert_eq!(receipt.version_id, "v1");

    // Every stored digest-bearing row must be byte-identical except
    // task-bound fields (task ids are per-database) and the version
    // publish stamp (envelope commits stamp at commit time).
    let dump = |repo: &SqliteCompactMarket, table: &str| {
        repo.read(|c| {
            let mut stmt = c.prepare(&format!("SELECT * FROM {table}")).unwrap();
            let names: Vec<String> = stmt.column_names().iter().map(|n| n.to_string()).collect();
            let mut rows = Vec::new();
            let mut iter = stmt.query([]).unwrap();
            while let Some(row) = iter.next().unwrap() {
                let mut obj = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value: rusqlite::types::Value = row.get(i).unwrap();
                    obj.insert(
                        name.clone(),
                        match value {
                            rusqlite::types::Value::Blob(b) => json!(hex_bytes(&b)),
                            rusqlite::types::Value::Integer(n) => json!(n),
                            rusqlite::types::Value::Text(t) => json!(t),
                            rusqlite::types::Value::Real(f) => json!(f),
                            rusqlite::types::Value::Null => Value::Null,
                        },
                    );
                }
                rows.push(Value::Object(obj));
            }
            Ok(json!(rows))
        })
        .unwrap()
    };
    for table in [
        "bar_segments",
        "segment_bars",
        "version_checkpoint_clips",
        "source_bindings",
        "evidence_bundles",
        "effective_heads",
    ] {
        let a = dump(&repo1, table);
        let b = dump(&repo2, table);
        assert_eq!(a, b, "table {table} differs between paths");
    }
    // publication_results / task_publications: identical except the
    // task-bound fields and the digest that binds them.
    let (pa, pb) = (
        dump(&repo1, "publication_results"),
        dump(&repo2, "publication_results"),
    );
    for (key, value) in pa[0].as_object().unwrap() {
        if key == "task_id" || key == "result_digest" {
            continue;
        }
        assert_eq!(value, &pb[0][key], "publication_results.{key} differs");
    }
    let (ta, tb) = (
        dump(&repo1, "task_publications"),
        dump(&repo2, "task_publications"),
    );
    for (key, value) in ta[0].as_object().unwrap() {
        if key == "task_id" {
            continue;
        }
        assert_eq!(value, &tb[0][key], "task_publications.{key} differs");
    }
    // The result digest is byte-equal under a shared task id.
    let digest_for = |repo: &SqliteCompactMarket, task: &str| {
        repo.read(|c| {
            let stored = compact_results::read_publication(c, "pub-1")?;
            let identity = compact_results::publication_identity_with_task(&stored, task);
            compact_market::publication_result_digest(&identity)
                .map(|d| hex_bytes(&d))
                .map_err(integrity)
        })
        .unwrap()
    };
    assert_eq!(digest_for(&repo1, "t"), digest_for(&repo2, "t"));
    // data_versions: all fields equal except published_at/integrity_digest.
    let rows_a = dump(&repo1, "data_versions");
    let rows_b = dump(&repo2, "data_versions");
    assert_eq!(rows_a.as_array().unwrap().len(), 1);
    let (va, vb) = (&rows_a[0], &rows_b[0]);
    for (key, value) in va.as_object().unwrap() {
        if key == "published_at" || key == "integrity_digest" {
            continue;
        }
        assert_eq!(value, &vb[key], "data_versions.{key} differs");
    }
    // integrity digests are equal once rebuilt under a shared stamp.
    let key_of = |repo: &SqliteCompactMarket| {
        repo.read(|c| {
            c.query_row(
                "SELECT version_key FROM data_versions WHERE version_id='v1'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap()
    };
    let stamp = "2025-01-02T00:00:00+00:00";
    let ia = repo1
        .read(|c| {
            compact_versions::recompute_integrity(c, key_of(&repo1), stamp).map(|d| hex_bytes(&d))
        })
        .unwrap();
    let ib = repo2
        .read(|c| {
            compact_versions::recompute_integrity(c, key_of(&repo2), stamp).map(|d| hex_bytes(&d))
        })
        .unwrap();
    assert_eq!(ia, ib, "integrity digests differ under shared stamp");
}

#[test]
fn collection_batch_records_item_failure_and_keeps_others() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    // Task with two items: one valid, one with unconfirmed units.
    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let scope2 = task_control::legacy::scope_id("sz.000001", "day");
    let work = FrozenWork {
        items: vec![
            ItemSpec {
                item_id: "item-a".into(),
                target: json!({"symbol": "sh.600006", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            },
            ItemSpec {
                item_id: "item-b".into(),
                target: json!({"symbol": "sz.000001", "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope2.clone()]),
            },
        ],
        scopes: BTreeSet::from([scope, scope2]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "collect_data".into(),
            dataset: "market.daily".into(),
            contract_version: "v1".into(),
        },
        intent: json!({}),
    };
    let snapshot = match repo
        .accept("req-batch", &command, &work, RUN, None)
        .unwrap()
        .record
        .outcome
    {
        task_control::RequestOutcome::Task(s) => s,
        _ => panic!("expected task"),
    };
    let task_id = snapshot.task.task_id.clone();
    // Claim the task (run_items does this in production).
    let head = repo.task_head(&task_id).unwrap();
    use crate::task_control::TaskRepository;
    repo.transition(&task_id, head.state_revision, RUN, Change::Claim)
        .unwrap();

    let mut good = collection_candidate_for(
        &mk_version("v-a", vec![bar(0, 100), bar(1, 110)]),
        &task_id,
        RUN,
    );
    good.item_id = "item-a".into();
    good.publication_id = "pub-a".into();
    let mut bad = collection_candidate_for(&mk_version("v-b", vec![bar(0, 100)]), &task_id, RUN);
    bad.item_id = "item-b".into();
    bad.symbol = "sz.000001".into();
    bad.publication_id = "pub-b".into();
    bad.unit_currency.price_unit = EvidenceValue::unknown("unconfirmed");

    let results = repo
        .commit_collection_batch(&task_id, &[good, bad])
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap().version_id, "v-a");
    assert!(results[1].is_err());
    let task = repo.task(&task_id).unwrap();
    assert_eq!(task.task.status, crate::task_control::Status::Running);
    assert_eq!(
        task.items
            .iter()
            .filter(|i| i.outcome.as_ref().is_some_and(|o| o.success))
            .count(),
        1
    );
    assert!(
        task.items
            .iter()
            .all(|i| i.stage == crate::task_control::Stage::Finished)
    );
    // The failed item produced no rows: only one published version exists.
    let heads = repo
        .read(|c| {
            c.query_row("SELECT count(*) FROM effective_heads", [], |r| {
                r.get::<_, i64>(0)
            })
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(heads, 1);
}

/// S5G-6 batch update commit: one transaction carries the claim, both
/// item starts, an append stage+commit and a no-change commit. Content
/// failures finish only their item; structural errors roll the whole
/// batch back.
#[test]
fn update_batch_commit_mixed_isolation_and_rollback() {
    use crate::market_update::{UpdateBatchItem, UpdateBatchPrepared};

    let dir = tempdir();
    let (_, repo) = repo(&dir);

    // Two published bases on distinct scopes.
    for (req, symbol, version_id, bars) in [
        (
            "req-b1",
            "sh.600006",
            "v1",
            vec![bar(0, 100), bar(1, 110), bar(2, 120)],
        ),
        ("req-b2", "sz.000001", "v1b", vec![bar(0, 200), bar(1, 210)]),
    ] {
        let scope = task_control::legacy::scope_id(symbol, "day");
        let work = FrozenWork {
            items: vec![ItemSpec {
                item_id: ITEM.into(),
                target: json!({"symbol": symbol, "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            }],
            scopes: BTreeSet::from([scope]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "collect_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        let task_id = match repo
            .accept(req, &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        };
        let revision = start_item(&repo, &task_id);
        let mut version = mk_version(version_id, bars);
        version.symbol = symbol.into();
        repo.stage(&envelope(&version, source("gen-1"), None))
            .unwrap();
        let checked = repo.candidate(version_id).unwrap();
        repo.mark_validated(&checked).unwrap();
        repo.commit(&PublishPlan {
            command: collect_command(&task_id, &format!("pub-{version_id}"), version_id, revision),
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();
    }

    // One update task carrying an item per scope.
    let accept_update = |req: &str, items: &[(&str, &str)]| -> String {
        let mut specs = Vec::new();
        let mut scopes = BTreeSet::new();
        for (item_id, symbol) in items {
            let scope = task_control::legacy::scope_id(symbol, "day");
            specs.push(ItemSpec {
                item_id: (*item_id).into(),
                target: json!({"symbol": symbol, "period": "day"}),
                progress_unit: "bars".into(),
                frozen_inputs: json!({}),
                scope_keys: BTreeSet::from([scope.clone()]),
            });
            scopes.insert(scope);
        }
        let work = FrozenWork {
            items: specs,
            scopes,
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: "v1".into(),
            },
            intent: json!({}),
        };
        match repo
            .accept(req, &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };
    let occupancy = |symbol: &str| {
        Scope {
            symbol: symbol.into(),
            period: Period::Day,
        }
        .occupancy_key()
    };
    let continuity =
        || json!({"continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}});

    // Mixed batch: append on one scope, no-change on the other — one
    // transaction performs claim + both item starts + both commits.
    let task_id = accept_update(
        "req-u1",
        &[("item-a", "sh.600006"), ("item-b", "sz.000001")],
    );
    let items = vec![
        UpdateBatchItem {
            task_id: task_id.clone(),
            item_id: "item-a".into(),
            publication_id: "upd-a".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: occupancy("sh.600006"),
            prepared: UpdateBatchPrepared::Append {
                base_version_id: "v1".into(),
                result_version_id: "v2".into(),
                upserts: vec![bar(3, 130)],
                continuity: continuity(),
            },
        },
        UpdateBatchItem {
            task_id: task_id.clone(),
            item_id: "item-b".into(),
            publication_id: "upd-b".into(),
            run_id: RUN.into(),
            symbol: "sz.000001".into(),
            occupancy_key: occupancy("sz.000001"),
            prepared: UpdateBatchPrepared::NoChange {
                base_version_id: "v1b".into(),
                range_start_day: 0,
                range_end_day: 3,
                continuity: continuity(),
            },
        },
    ];
    let results = repo.commit_update_batch(&task_id, &items).unwrap();
    assert_eq!(results.len(), 2);
    let receipt_a = results[0].as_ref().unwrap();
    let receipt_b = results[1].as_ref().unwrap();
    assert_eq!(receipt_a.version_id, "v2");
    assert_eq!(receipt_b.version_id, "v1b");
    let task = repo.task(&task_id).unwrap();
    assert_eq!(task.task.status, crate::task_control::Status::Succeeded);
    assert!(
        task.items
            .iter()
            .all(|i| i.stage == crate::task_control::Stage::Finished)
    );
    let page = repo
        .range(
            &Scope {
                symbol: "sh.600006".into(),
                period: Period::Day,
            },
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v2");
    assert_eq!(page.rows.len(), 4);
    let page = repo
        .range(
            &Scope {
                symbol: "sz.000001".into(),
                period: Period::Day,
            },
            None,
            &Range {
                start: date(0),
                end: date(10),
                after: None,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(page.version.version_id, "v1b");
    assert_eq!(page.rows.len(), 2);

    // Replay of the committed batch returns the same receipts without
    // touching task state.
    let replayed = repo.commit_update_batch(&task_id, &items).unwrap();
    assert_eq!(replayed[0].as_ref().unwrap(), receipt_a);
    assert_eq!(replayed[1].as_ref().unwrap(), receipt_b);

    // Content failure isolation: a duplicate-day upsert fails its
    // savepoint; the sibling no-change item still commits.
    let task_id2 = accept_update(
        "req-u2",
        &[("item-c", "sh.600006"), ("item-d", "sz.000001")],
    );
    let items2 = vec![
        UpdateBatchItem {
            task_id: task_id2.clone(),
            item_id: "item-c".into(),
            publication_id: "upd-c".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: occupancy("sh.600006"),
            prepared: UpdateBatchPrepared::Append {
                base_version_id: "v2".into(),
                result_version_id: "v3".into(),
                upserts: vec![bar(4, 140), bar(4, 141)],
                continuity: continuity(),
            },
        },
        UpdateBatchItem {
            task_id: task_id2.clone(),
            item_id: "item-d".into(),
            publication_id: "upd-d".into(),
            run_id: RUN.into(),
            symbol: "sz.000001".into(),
            occupancy_key: occupancy("sz.000001"),
            prepared: UpdateBatchPrepared::NoChange {
                base_version_id: "v1b".into(),
                range_start_day: 0,
                range_end_day: 3,
                continuity: continuity(),
            },
        },
    ];
    let results2 = repo.commit_update_batch(&task_id2, &items2).unwrap();
    assert!(results2[0].is_err());
    assert_eq!(results2[1].as_ref().unwrap().version_id, "v1b");
    let task2 = repo.task(&task_id2).unwrap();
    assert_eq!(
        task2.task.status,
        crate::task_control::Status::PartiallyFailed
    );
    let item_c = task2
        .items
        .iter()
        .find(|i| i.spec.item_id == "item-c")
        .unwrap();
    assert_eq!(item_c.stage, crate::task_control::Stage::Finished);
    assert!(!item_c.outcome.as_ref().unwrap().success);
    // The failed append left no staged rows: no v3 version exists.
    let v3_rows: i64 = repo
        .read(|c| {
            c.query_row(
                "SELECT count(*) FROM data_versions WHERE version_id='v3'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(v3_rows, 0);

    // Structural error (an item absent from the task) aborts the whole
    // batch: even the sibling's savepoint-released writes roll back and
    // the claim never lands.
    let task_id3 = accept_update("req-u3", &[("item-e", "sh.600006")]);
    let items3 = vec![
        UpdateBatchItem {
            task_id: task_id3.clone(),
            item_id: "item-e".into(),
            publication_id: "upd-e".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: occupancy("sh.600006"),
            prepared: UpdateBatchPrepared::NoChange {
                base_version_id: "v2".into(),
                range_start_day: 0,
                range_end_day: 4,
                continuity: continuity(),
            },
        },
        UpdateBatchItem {
            task_id: task_id3.clone(),
            item_id: "ghost".into(),
            publication_id: "upd-ghost".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: occupancy("sh.600006"),
            prepared: UpdateBatchPrepared::NoChange {
                base_version_id: "v2".into(),
                range_start_day: 0,
                range_end_day: 4,
                continuity: continuity(),
            },
        },
    ];
    assert!(repo.commit_update_batch(&task_id3, &items3).is_err());
    let task3 = repo.task(&task_id3).unwrap();
    assert_eq!(task3.task.status, crate::task_control::Status::Pending);
    assert!(
        task3
            .items
            .iter()
            .all(|i| i.stage == crate::task_control::Stage::Pending)
    );

    // After the rollback the same task commits cleanly on retry.
    let retry = repo.commit_update_batch(&task_id3, &items3[..1]).unwrap();
    assert_eq!(retry[0].as_ref().unwrap().version_id, "v2");
    assert_eq!(
        repo.task(&task_id3).unwrap().task.status,
        crate::task_control::Status::Succeeded
    );
}

#[test]
fn fixed_window_and_facts_read_without_full_materialization() {
    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let ymd = |y, m, d| compact_market::epoch_day(NaiveDate::from_ymd_opt(y, m, d).unwrap());
    let days = [
        ymd(2024, 6, 14),
        ymd(2025, 3, 10),
        ymd(2025, 11, 20),
        ymd(2026, 1, 5),
    ];
    let base = mk_version(
        "v1",
        days.iter()
            .enumerate()
            .map(|(i, d)| bar(*d, 100 + i as i64))
            .collect(),
    );
    repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
    let checked = repo.candidate("v1").unwrap();
    repo.mark_validated(&checked).unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: checked.clone(),
        required_heads: vec![(Scope::of(&checked.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // `fixed_facts` is the typed row projection: identity + stored digest.
    let facts = repo.fixed_facts("v1").unwrap();
    assert_eq!(facts.version_id, "v1");
    assert_eq!(facts.symbol, "sh.600006");
    assert_eq!(facts.period, Period::Day);
    let stored_integrity: String = repo
        .read(|c| {
            c.query_row(
                "SELECT lower(hex(integrity_digest)) FROM data_versions WHERE version_id='v1'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(facts.integrity_sha256, stored_integrity);

    // Published reads report the stored typed digest, not a legacy envelope
    // recompute — that is what every frozen digest comparison binds to.
    let fixed = repo.fixed("v1").unwrap();
    assert_eq!(fixed.raw()["integrity_sha256"], json!(stored_integrity));

    // Windowed read: only in-window bars, still the stored digest.
    let window = repo
        .fixed_window(
            "v1",
            NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 12, 31).unwrap(),
        )
        .unwrap();
    assert_eq!(window.raw()["integrity_sha256"], json!(stored_integrity));
    assert_eq!(window.version().version_id, "v1");
    assert_eq!(window.version().symbol, "sh.600006");
    let expected: Vec<Bar> = days[1..3]
        .iter()
        .enumerate()
        .map(|(i, d)| bar(*d, 101 + i as i64))
        .collect();
    assert_eq!(window.version().records, expected);
    assert_eq!(window.batch.bars, expected);
    // A window covering nothing yields an empty projection, not an error.
    let empty = repo
        .fixed_window(
            "v1",
            NaiveDate::from_ymd_opt(2020, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2020, 12, 31).unwrap(),
        )
        .unwrap();
    assert!(empty.version().records.is_empty());
    assert_eq!(empty.raw()["integrity_sha256"], json!(stored_integrity));

    // Windowed read on a non-published version is NotFound, matching `fixed`.
    let staged = mk_version("v-staged", vec![bar(days[0], 100)]);
    repo.stage(&envelope(&staged, source("gen-1"), None))
        .unwrap();
    assert!(matches!(
        repo.fixed_window(
            "v-staged",
            NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2024, 12, 31).unwrap(),
        ),
        Err(Error::NotFound)
    ));
    assert!(matches!(repo.fixed_facts("v-staged"), Err(Error::NotFound)));
    assert!(matches!(repo.fixed_facts("missing"), Err(Error::NotFound)));
}

/// Manual timing probe on a real capacity DB (#262 yearly read path):
/// `X_CAPACITY_DB=/path/to/market.db cargo test --release fixed_window_timing -- --ignored --nocapture`
#[test]
#[ignore = "manual probe against a populated capacity DB"]
fn fixed_window_timing_probe() {
    use std::time::Instant;
    let path = PathBuf::from(std::env::var("X_CAPACITY_DB").expect("X_CAPACITY_DB"));
    let repo = SqliteCompactMarket::open(&path).unwrap();
    // Deepest histories first (most bars), plus a broad sample.
    let ids: Vec<String> = repo
        .read(|c| {
            c.prepare(
                "SELECT version_id FROM data_versions \
                 WHERE state=3 AND period=1 ORDER BY record_count DESC LIMIT 10",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
        })
        .unwrap();
    let sample: Vec<String> = repo
        .read(|c| {
            c.prepare(
                "SELECT version_id FROM data_versions \
                 WHERE state=3 AND period=1 ORDER BY version_key LIMIT 40",
            )
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
        })
        .unwrap();
    let all: Vec<String> = ids.into_iter().chain(sample).collect();
    let first = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let last = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
    let mut full_ms = Vec::new();
    let mut window_ms = Vec::new();
    let mut bars_full = 0usize;
    let mut bars_window = 0usize;
    for id in &all {
        let t = Instant::now();
        let r = repo.fixed(id).unwrap();
        full_ms.push(t.elapsed().as_secs_f64() * 1e3);
        bars_full += r.version().records.len();
        let t = Instant::now();
        let w = repo.fixed_window(id, first, last).unwrap();
        window_ms.push(t.elapsed().as_secs_f64() * 1e3);
        bars_window += w.version().records.len();
        // Digest consistency: the windowed projection reports the stored
        // digest of the complete version.
        assert_eq!(w.raw()["integrity_sha256"], r.raw()["integrity_sha256"]);
    }
    let total_full: f64 = full_ms.iter().sum();
    let total_window: f64 = window_ms.iter().sum();
    println!(
        "n={} full={total_full:.1}ms window={total_window:.1}ms ratio={:.1}x bars_full={bars_full} bars_window={bars_window}",
        all.len(),
        total_full / total_window,
    );
    let mut sorted = full_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "full p50={:.1}ms p90={:.1}ms max={:.1}ms",
        sorted[sorted.len() / 2],
        sorted[(sorted.len() as f64 * 0.9) as usize],
        sorted[sorted.len() - 1]
    );
    let mut sorted = window_ms.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "window p50={:.1}ms p90={:.1}ms max={:.1}ms",
        sorted[sorted.len() / 2],
        sorted[(sorted.len() as f64 * 0.9) as usize],
        sorted[sorted.len() - 1]
    );
}

/// Local synthetic SQLite path, stub source verification, no MCP or network.
#[test]
#[ignore = "manual release pipeline timing on fresh tmpfs databases"]
fn yearly_pipeline_timing_probe() {
    use std::time::Instant;
    let mut samples = Vec::new();
    for _ in 0..10 {
        use crate::market_update::tasks::Operation;
        use crate::task_control::{Contract, RequestOutcome};
        use crate::yearly_publication::{
            DerivationEvidence,
            runner::{YearlyExecution, YearlyRunner, YearlyVerifier},
            tasks::YearlyContract,
        };

        struct Stub;
        impl YearlyVerifier for Stub {
            fn verify(
                &self,
                base: &CheckedRecord,
                years: &BTreeSet<i32>,
                _annual: &crate::yearly::AnnualSeries,
                derived_at: DateTime<Utc>,
            ) -> Result<DerivationEvidence, crate::market_data::SourceError> {
                Ok(DerivationEvidence {
                    base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
                    years: years.clone(),
                    as_of: derived_at
                        .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                        .date_naive(),
                    coverage: CoverageEvidence::complete("stub re-aggregation verified"),
                })
            }
        }

        let dir = tempdir();
        let (_, repo) = repo(&dir);
        // Publish a daily base covering 2025 and 2026 through the real path.
        let task_id = accept_task(&repo);
        let revision = start_item(&repo, &task_id);
        let last = NaiveDate::from_ymd_opt(2026, 9, 18).unwrap();
        let days =
            (0..10000).map(|i| compact_market::epoch_day(last - chrono::Duration::days(9999 - i)));
        let version = mk_version(
            "v1",
            days.enumerate()
                .map(|(i, d)| bar(d, 100 + i as i64))
                .collect(),
        );
        repo.stage(&envelope(&version, source("gen-1"), None))
            .unwrap();
        let candidate = repo.candidate("v1").unwrap();
        repo.mark_validated(&candidate).unwrap();
        repo.commit(&PublishPlan {
            command: collect_command(&task_id, "pub-1", "v1", revision),
            record: candidate.clone(),
            required_heads: vec![(Scope::of(&candidate.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();

        // Accept the yearly task through the real contract, then run the item.
        let contract = YearlyContract {
            operation: Operation::Update,
            repository: repo.clone(),
            today: || NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        };
        let acceptance_started = Instant::now();
        let work = contract
            .resolve(&json!({"items":[{
                "symbol":"sh.600006",
                "base_daily_version_id":"v1",
                "years":[2025,2026],
                "intent":{"CurrentHead":{"expected_daily_head":"v1","expected_yearly_head":null}},
                "comparison_version_id":null
            }]}))
            .unwrap();
        let command = Command {
            key: contract.key(),
            intent: json!({}),
        };
        let acceptance = repo
            .accept("req-yearly", &command, &work, RUN, None)
            .unwrap();
        let RequestOutcome::Task(snapshot) = acceptance.record.outcome else {
            panic!("expected task outcome")
        };
        let yearly_task = snapshot.task.task_id.clone();
        let item_id = snapshot.items[0].spec.item_id.clone();

        let runner = YearlyRunner {
            repository: repo.clone(),
            run_id: RUN.into(),
            derived_at: ts(),
        };
        let accept_ms = acceptance_started.elapsed().as_secs_f64() * 1000.0;
        let execution_started = Instant::now();
        let outcome = runner.run_item(&yearly_task, &item_id, &Stub).unwrap();
        let YearlyExecution::Published(receipt) = outcome else {
            let snap = repo.task(&yearly_task).unwrap();
            panic!("yearly item did not publish: {:?}", snap.items[0].outcome)
        };

        let execution_ms = execution_started.elapsed().as_secs_f64() * 1000.0;
        samples.push((accept_ms, execution_ms));
        let fixed = repo.fixed(&receipt.version_id).unwrap();
        assert_eq!(fixed.version.period, Period::Year);
        assert_eq!(fixed.version.records.len(), 2);
        assert_eq!(
            fixed
                .version
                .yearly_derivation
                .as_ref()
                .unwrap()
                .base_daily_version_id,
            "v1"
        );
        // A derived version inherits the published base's source binding.
        assert_eq!(
            fixed.raw()["compact_source"]["generation_id"],
            json!("gen-1")
        );
        let head = repo
            .versions(&Scope {
                symbol: "sh.600006".into(),
                period: Period::Year,
            })
            .unwrap();
        assert_eq!(head.len(), 1);
        assert_eq!(head[0].version_id, receipt.version_id);

        // Re-running the committed item replays the stored receipt.
        let YearlyExecution::Published(replay) =
            runner.run_item(&yearly_task, &item_id, &Stub).unwrap()
        else {
            panic!("yearly replay did not publish")
        };
        assert_eq!(replay, receipt);

        // The audit projection requires the stored result to link its daily
        // derivation base (schema-19 task-progress reads hit this path).
        let (audit, _) = repo
            .read(|c| {
                compact_results::read_yearly_audit(c, &receipt.publication_id).map(|o| o.unwrap())
            })
            .unwrap();
        assert_eq!(audit["base_daily_version_id"], json!("v1"));
    }
    let mut accept: Vec<f64> = samples.iter().map(|s| s.0).collect();
    let mut execution: Vec<f64> = samples.iter().map(|s| s.1).collect();
    accept.sort_by(f64::total_cmp);
    execution.sort_by(f64::total_cmp);
    eprintln!(
        "pipeline fresh_dbs=10 daily_bars=10000 years=2 accept_p50_ms={:.3} execution_p50_ms={:.3} execution_max_ms={:.3}",
        accept[5], execution[5], execution[9]
    );
}
