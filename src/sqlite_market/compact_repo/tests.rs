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
        .initialize_compact()
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

/// #320 review: when the source tail has an out-of-range record, the
/// published coverage stays at the base claim. Stage that metadata through
/// the typed delta path and the envelope merge in two identical stores; the
/// resulting candidate envelopes must be byte-identical.
#[test]
fn typed_append_base_coverage_parity_with_envelope_bytes() {
    let publish_base = |repo: &SqliteCompactMarket| {
        let task_id = accept_task(repo);
        let revision = start_item(repo, &task_id);
        let base = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
        repo.stage(&envelope(&base, source("gen-1"), None)).unwrap();
        let checked = repo.candidate("v1").unwrap();
        repo.mark_validated(&checked).unwrap();
        repo.commit(&PublishPlan {
            command: collect_command(&task_id, "pub-base", "v1", revision),
            record: checked.clone(),
            required_heads: vec![(Scope::of(&checked.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();
        checked
    };

    let typed_dir = tempdir();
    let (_, typed_repo) = repo(&typed_dir);
    let typed_base = publish_base(&typed_repo);
    let source_b = json!({
        "generation_id": "gen-b",
        "generation_manifest_sha256": "bc".repeat(32),
        "day_member_sha256": "de".repeat(32),
        "security_event_sha256": "ef".repeat(32),
        "generation_evidence": {
            "manifest_sha256": "bc".repeat(32),
            "snapshot_inputs": [
                {"role": "day", "member": "sh/lday/sh600006.day",
                 "archive_sha256": "12".repeat(32), "member_sha256": "de".repeat(32)},
                {"role": "gbbq-security-events", "member": "gbbq#sh.600006",
                 "archive_sha256": "34".repeat(32), "member_sha256": "ef".repeat(32)}
            ]
        }
    });
    let metadata = crate::market_update::TypedVersionMetadata {
        generation_id: source_b["generation_id"].as_str().unwrap().into(),
        generation_manifest_sha256: source_b["generation_manifest_sha256"]
            .as_str()
            .unwrap()
            .into(),
        day_member_sha256: source_b["day_member_sha256"].as_str().unwrap().into(),
        security_event_sha256: source_b["security_event_sha256"].as_str().unwrap().into(),
        generation_evidence: source_b["generation_evidence"].clone(),
        source: "official".into(),
        coverage: typed_base.batch.coverage.clone(),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    };
    typed_repo
        .stage_ops_delta_metadata(
            "sh.600006",
            "v1",
            "v2",
            &[bar(3, 130)],
            &[],
            Some(&metadata),
        )
        .unwrap();
    let typed_candidate = typed_repo.candidate("v2").unwrap();

    let envelope_dir = tempdir();
    let (_, envelope_repo) = repo(&envelope_dir);
    publish_base(&envelope_repo);
    let mut combined = mk_version(
        "v2",
        vec![bar(0, 100), bar(1, 110), bar(2, 120), bar(3, 130)],
    );
    combined.published_at = typed_candidate.version().published_at;
    envelope_repo
        .stage(&envelope(&combined, source_b, Some("v1")))
        .unwrap();
    let envelope_candidate = envelope_repo.candidate("v2").unwrap();

    assert_eq!(
        serde_json::to_vec(typed_candidate.raw()).unwrap(),
        serde_json::to_vec(envelope_candidate.raw()).unwrap()
    );
}

#[test]
fn legacy_adapters_reject_compact_schema() {
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
        repo.commit_range_ops_typed(&bad_command, &scope, "v1", &[bar(3, 130)], &[], None, None)
            .is_err()
    );

    let receipt = repo
        .commit_range_ops_typed(
            &command,
            &scope,
            "v1",
            &[bar(3, 130)],
            &[],
            None,
            Some(&evidence),
        )
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
        .commit_range_ops_typed(&command, &scope, "v1", &[bar(3, 130)], &[], None, None)
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

/// #320: a proven append may rebind the delta version to a new generation
/// without materializing the merged envelope; metadata drift beyond the
/// typed contract stays rejected instead of silently overriding the base.
#[test]
fn typed_append_rebinds_generation_metadata_and_rejects_drift() {
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
    assert_eq!(
        repo.task(&task_id).unwrap().task.status,
        crate::task_control::Status::Succeeded
    );

    let gen_b = "be".repeat(32);
    let day_b = "12".repeat(32);
    let events = "ef".repeat(32);
    let metadata = crate::market_update::TypedVersionMetadata {
        generation_id: gen_b.clone(),
        generation_manifest_sha256: gen_b.clone(),
        day_member_sha256: day_b.clone(),
        security_event_sha256: events.clone(),
        generation_evidence: json!({
            "manifest_sha256": gen_b.clone(),
            "snapshot_inputs": [
                {"role": "day", "member": "sh/lday/sh600006.day",
                 "archive_sha256": "34".repeat(32), "member_sha256": day_b.clone()},
                {"role": "gbbq-security-events", "member": "gbbq#sh.600006",
                 "archive_sha256": "56".repeat(32), "member_sha256": events.clone()}
            ],
        }),
        source: "official".into(),
        coverage: CoverageEvidence::complete("complete .day history from generation B"),
        unit_currency: units(),
        adjustment_evidence: AdjustmentEvidence::confirmed("qfq", "bound"),
    };

    let accept_update = |req: &str| -> String {
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
            .accept(req, &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        }
    };

    let task_id2 = accept_update("req-u2");
    let revision2 = start_item(&repo, &task_id2);
    let scope = Scope::of(&base_checked.version);
    let command = Publication {
        publication_id: "upd-meta".into(),
        version_id: "v2".into(),
        task_id: task_id2.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision2,
        intent: PublishIntent::Daily {
            expected_head: Some("v1".into()),
        },
    };
    let receipt = repo
        .commit_range_ops_typed(
            &command,
            &scope,
            "v1",
            &[bar(3, 130)],
            &[],
            Some(&metadata),
            None,
        )
        .unwrap()
        .expect("typed metadata append supported");
    assert_eq!(receipt.version_id, "v2");
    let head = repo.fixed("v2").unwrap();
    assert_eq!(head.version().records.len(), 4);
    assert!(head.batch.coverage.reason.contains("generation B"));
    let bound_generation: String = repo
        .read(|c| {
            c.query_row(
                "SELECT sb.generation_id FROM data_versions v \
                 JOIN source_bindings sb ON sb.source_binding_digest=v.source_binding_digest \
                 WHERE v.version_id='v2'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(bound_generation, gen_b);

    // The same API rejects metadata that would change the proven event set
    // instead of treating it as an opaque override. Generation, manifest
    // and day member are all fresh so the ONLY drift is the security-event
    // digest — otherwise the base-identity guards would reject first and
    // the event guard would never be exercised.
    let task_id3 = accept_update("req-u3");
    let revision3 = start_item(&repo, &task_id3);
    let mut bad = metadata.clone();
    bad.generation_id = "cf".repeat(32);
    bad.generation_manifest_sha256 = "cf".repeat(32);
    bad.day_member_sha256 = "13".repeat(32);
    bad.security_event_sha256 = "aa".repeat(32);
    bad.generation_evidence["manifest_sha256"] = json!(bad.generation_manifest_sha256.clone());
    bad.generation_evidence["snapshot_inputs"][0]["member_sha256"] =
        json!(bad.day_member_sha256.clone());
    bad.generation_evidence["snapshot_inputs"][1]["member_sha256"] =
        json!(bad.security_event_sha256.clone());
    let bad_command = Publication {
        publication_id: "upd-meta-bad".into(),
        version_id: "v-bad".into(),
        task_id: task_id3.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision3,
        intent: PublishIntent::Daily {
            expected_head: Some("v2".into()),
        },
    };
    let err = repo
        .commit_range_ops_typed(
            &bad_command,
            &scope,
            "v2",
            &[bar(4, 140)],
            &[],
            Some(&bad),
            None,
        )
        .expect_err("event digest drift must be rejected");
    assert!(
        matches!(err, crate::market_storage::Error::Invalid(_))
            && err
                .to_string()
                .contains("does not match the proven member continuity"),
        "unexpected rejection: {err}"
    );
    let revision3 = repo
        .transition(
            &task_id3,
            revision3,
            RUN,
            Change::FinishItem(
                ITEM.into(),
                crate::task_control::ItemOutcome {
                    success: false,
                    error: Some("typed metadata rejected".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap()
        .task
        .state_revision;
    repo.transition(&task_id3, revision3, RUN, Change::FinishExecution)
        .unwrap();

    // Conflicting unit semantics likewise cannot be smuggled through the
    // metadata channel.
    let task_id4 = accept_update("req-u4");
    let revision4 = start_item(&repo, &task_id4);
    let mut bad_units = metadata.clone();
    bad_units.day_member_sha256 = "78".repeat(32);
    bad_units.generation_evidence["snapshot_inputs"][0]["member_sha256"] =
        json!(bad_units.day_member_sha256.clone());
    bad_units.unit_currency.price_unit = EvidenceValue::confirmed("fen", "drift");
    let bad_units_command = Publication {
        publication_id: "upd-meta-units".into(),
        version_id: "v-bad-units".into(),
        task_id: task_id4.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision4,
        intent: PublishIntent::Daily {
            expected_head: Some("v2".into()),
        },
    };
    let err = repo
        .commit_range_ops_typed(
            &bad_units_command,
            &scope,
            "v2",
            &[bar(4, 140)],
            &[],
            Some(&bad_units),
            None,
        )
        .expect_err("unit drift must be rejected");
    assert!(
        matches!(err, crate::market_storage::Error::Invalid(_))
            && err
                .to_string()
                .contains("metadata drift exceeds the ops contract"),
        "unexpected rejection: {err}"
    );
    let revision4 = repo
        .transition(
            &task_id4,
            revision4,
            RUN,
            Change::FinishItem(
                ITEM.into(),
                crate::task_control::ItemOutcome {
                    success: false,
                    error: Some("typed metadata rejected".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap()
        .task
        .state_revision;
    repo.transition(&task_id4, revision4, RUN, Change::FinishExecution)
        .unwrap();

    let task_cov = accept_update("req-u-coverage");
    let revision_cov = start_item(&repo, &task_cov);
    let mut bad_coverage = metadata.clone();
    bad_coverage.coverage =
        crate::market_data::CoverageEvidence::incomplete("coverage evidence incomplete");
    let bad_coverage_command = Publication {
        publication_id: "upd-meta-coverage".into(),
        version_id: "v-bad-coverage".into(),
        task_id: task_cov.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision_cov,
        intent: PublishIntent::Daily {
            expected_head: Some("v2".into()),
        },
    };
    let err = repo
        .commit_range_ops_typed(
            &bad_coverage_command,
            &scope,
            "v2",
            &[bar(4, 140)],
            &[],
            Some(&bad_coverage),
            None,
        )
        .expect_err("non-complete coverage must be rejected");
    assert!(
        matches!(err, crate::market_storage::Error::Invalid(_))
            && err
                .to_string()
                .contains("metadata drift exceeds the ops contract"),
        "unexpected rejection: {err}"
    );
    let revision_cov = repo
        .transition(
            &task_cov,
            revision_cov,
            RUN,
            Change::FinishItem(
                ITEM.into(),
                crate::task_control::ItemOutcome {
                    success: false,
                    error: Some("typed metadata rejected".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap()
        .task
        .state_revision;
    repo.transition(&task_cov, revision_cov, RUN, Change::FinishExecution)
        .unwrap();

    let task_id5 = accept_update("req-u5");
    let revision5 = start_item(&repo, &task_id5);
    let mut metadata_c = metadata.clone();
    metadata_c.generation_id = "cf".repeat(32);
    metadata_c.generation_manifest_sha256 = metadata_c.generation_id.clone();
    metadata_c.day_member_sha256 = "13".repeat(32);
    metadata_c.generation_evidence["manifest_sha256"] = json!(metadata_c.generation_id.clone());
    metadata_c.generation_evidence["snapshot_inputs"][0]["member_sha256"] =
        json!(metadata_c.day_member_sha256.clone());
    repo.stage_ops_delta_metadata("sh.600006", "v2", "v3-staged", &[bar(4, 140)], &[], None)
        .unwrap();
    let staged_command = Publication {
        publication_id: "upd-meta-staged".into(),
        version_id: "v3-staged".into(),
        task_id: task_id5.clone(),
        item_id: ITEM.into(),
        run_id: RUN.into(),
        expected_revision: revision5,
        intent: PublishIntent::Daily {
            expected_head: Some("v2".into()),
        },
    };
    assert!(
        repo.commit_range_ops_typed(
            &staged_command,
            &scope,
            "v2",
            &[bar(4, 140)],
            &[],
            Some(&metadata_c),
            None,
        )
        .is_err()
    );
    let revision5 = repo
        .transition(
            &task_id5,
            revision5,
            RUN,
            Change::FinishItem(
                ITEM.into(),
                crate::task_control::ItemOutcome {
                    success: false,
                    error: Some("staged metadata rejected".into()),
                    evidence: None,
                },
            ),
        )
        .unwrap()
        .task
        .state_revision;
    repo.transition(&task_id5, revision5, RUN, Change::FinishExecution)
        .unwrap();

    // The successful result is still an append delta on the expected head.
    let mode: i64 = repo
        .read(|c| {
            c.query_row(
                "SELECT manifest_mode FROM data_versions WHERE version_id='v2'",
                [],
                |r| r.get(0),
            )
            .map_err(storage)
        })
        .unwrap();
    assert_eq!(mode, 2);
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
        "evidence_parts",
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
                metadata: None,
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
                metadata: None,
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

#[test]
#[ignore = "manual stage-level timing probe (#278); synthetic 10k-bar base"]
fn yearly_stage_timing_probe() {
    use crate::market_update::tasks::Operation;
    use crate::sqlite_market::Publisher;
    use crate::task_control::{Contract, RequestOutcome};
    use crate::yearly_publication::{
        Derivation, DerivationEvidence,
        results::YearlyRepository,
        runner::{YearlyRunner, YearlyVerifier},
        tasks::YearlyContract,
    };
    use std::time::Instant;

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

    // Stage names mirror YearlyRunner::derive_and_commit and the surrounding
    // run_item transitions; drift check: keep in sync with runner.rs.
    const STAGES: [&str; 8] = [
        "binding+receipt",
        "claim+start_item",
        "fixed_window",
        "aggregate",
        "verify",
        "task_recheck",
        "prepare_resid",
        "commit(+item close)",
    ];
    let mut timings: [Vec<f64>; 8] = Default::default();
    let mut run_item_ms = Vec::new();
    let mut derive_only = Vec::new();
    let mut binding_ms = Vec::new();
    let mut facts_ms = Vec::new();
    let mut window1d_ms = Vec::new();
    let mut cand_miss_ms = Vec::new();
    let mut stage_ms = Vec::new();
    let mut cand_hit_ms = Vec::new();
    let mut validate_ms = Vec::new();
    let mut pack_ms = Vec::new();

    for _ in 0..10 {
        let dir = tempdir();
        let (_, repo) = repo(&dir);
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

        // --- stage replication of run_item/derive_and_commit ---
        let t = Instant::now();
        let mut binding = repo
            .task_item_binding(&yearly_task, &item_id)
            .unwrap()
            .unwrap();
        assert!(
            repo.yearly_item_receipt(&yearly_task, &item_id)
                .unwrap()
                .is_none()
        );
        timings[0].push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        repo.transition_task_head(&mut binding.head, None, RUN, Change::Claim)
            .unwrap();
        repo.transition_task_head(
            &mut binding.head,
            Some(&mut binding.item),
            RUN,
            Change::StartItem(item_id.clone()),
        )
        .unwrap();
        timings[1].push(t.elapsed().as_secs_f64() * 1000.0);

        let frozen = &binding.item.spec.frozen_inputs;
        let base_id = frozen["base_daily_version_id"].as_str().unwrap();
        let years: BTreeSet<i32> = serde_json::from_value(frozen["years"].clone()).unwrap();
        let (first_day, last_day) = (
            NaiveDate::from_ymd_opt(*years.first().unwrap(), 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(*years.last().unwrap(), 12, 31).unwrap(),
        );

        let t = Instant::now();
        let base = repo.fixed_window(base_id, first_day, last_day).unwrap();
        timings[2].push(t.elapsed().as_secs_f64() * 1000.0);

        let as_of = ts()
            .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
            .date_naive();
        let t = Instant::now();
        let annual = crate::yearly::aggregate(base.version(), Some(&years), as_of).unwrap();
        timings[3].push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let evidence = Stub.verify(&base, &years, &annual, ts()).unwrap();
        timings[4].push(t.elapsed().as_secs_f64() * 1000.0);

        let t = Instant::now();
        let _recheck = repo.task_head(&yearly_task).unwrap();
        timings[5].push(t.elapsed().as_secs_f64() * 1000.0);

        let id = crate::hash::sha256(json!([yearly_task, item_id]).to_string().as_bytes());
        let request = Derivation {
            symbol: base.version().symbol.clone(),
            base_daily_version_id: base_id.into(),
            version_id: format!("yearly-version-{id}"),
            years: years.clone(),
            derived_at: ts(),
        };
        let intent: PublishIntent = serde_json::from_value(frozen["intent"].clone()).unwrap();
        let publisher = Publisher {
            repository: repo.clone(),
        };
        let t = Instant::now();
        let cand = publisher
            .derive_yearly(&request, &evidence, &base, &annual)
            .unwrap();
        let derive_ms = t.elapsed().as_secs_f64() * 1000.0;
        derive_only.push(derive_ms);
        // Constituent-call costs inside prepare_yearly_result (#278).
        let t = Instant::now();
        let _b = repo.task_item_binding(&yearly_task, &item_id).unwrap();
        binding_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let _f = repo.fixed_facts(base_id).unwrap();
        facts_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let f0 = _f.first_day;
        let _w = repo.fixed_window(base_id, f0, f0).unwrap();
        window1d_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        // Granular constituent costs of prepare_derived_yearly (#278).
        let vid = request.version_id.clone();
        let t = Instant::now();
        assert!(matches!(repo.candidate(&vid), Err(Error::NotFound)));
        cand_miss_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        repo.stage(cand.raw()).unwrap();
        stage_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let staged = repo.candidate(&vid).unwrap();
        cand_hit_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        repo.mark_validated(&staged).unwrap();
        validate_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let _pack = crate::sqlite_market::envelope::pack(cand.raw()).unwrap();
        pack_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let plan = publisher
            .prepare_yearly_result_prefetched(
                Publication {
                    publication_id: format!("yearly-result-{id}"),
                    version_id: request.version_id.clone(),
                    task_id: binding.head.task_id.clone(),
                    item_id: item_id.clone(),
                    run_id: RUN.into(),
                    expected_revision: binding.head.state_revision,
                    intent,
                },
                &cand,
                None,
                Some(&binding),
                Some(&base),
            )
            .unwrap();
        timings[6].push(
            t.elapsed().as_secs_f64() * 1000.0
                - derive_ms
                - binding_ms.last().unwrap()
                - facts_ms.last().unwrap()
                - window1d_ms.last().unwrap(),
        );

        let t = Instant::now();
        let receipt = repo.commit_yearly(&plan).unwrap();
        timings[7].push(t.elapsed().as_secs_f64() * 1000.0);

        // End-to-end sanity: re-run path replays stored receipt.
        let runner = YearlyRunner {
            repository: repo.clone(),
            run_id: RUN.into(),
            derived_at: ts(),
        };
        let t = Instant::now();
        assert!(matches!(
            runner.run_item(&yearly_task, &item_id, &Stub).unwrap(),
            crate::yearly_publication::runner::YearlyExecution::Published(_)
        ));
        run_item_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let _ = receipt;
    }

    eprintln!("stage probe fresh_dbs=10 daily_bars=10000 years=2");
    for (i, name) in STAGES.iter().enumerate() {
        let mut v = timings[i].clone();
        v.sort_by(f64::total_cmp);
        eprintln!("  {name:>18}: p50={:.3}ms max={:.3}ms", v[5], v[9]);
    }
    for (name, v) in [
        ("derive_yearly", &derive_only),
        ("task_item_binding", &binding_ms),
        ("fixed_facts", &facts_ms),
        ("fixed_window_1d", &window1d_ms),
        ("candidate_miss", &cand_miss_ms),
        ("stage", &stage_ms),
        ("candidate_hit", &cand_hit_ms),
        ("mark_validated", &validate_ms),
        ("envelope_pack", &pack_ms),
    ] {
        let mut v = v.clone();
        v.sort_by(f64::total_cmp);
        eprintln!("  {name:>18}: p50={:.3}ms max={:.3}ms", v[5], v[9]);
    }
    run_item_ms.sort_by(f64::total_cmp);
    eprintln!("  run_item_replay: p50={:.3}ms", run_item_ms[5]);
}

/// Real-DB stage probe (#278): runs the yearly item pipeline against an
/// isolated *copy* of the production database (never the live path). Items
/// mirror the preview.24 shape — `years` = [2025,2026] window presence is
/// checked per entity, intent is CurrentHead bound to the stored head in
/// `nochange` mode or HistoricalFixedBase in `publish` mode — and the same
/// stage decomposition as `yearly_stage_timing_probe` is reported.
///
///   X_DATA_PROBE_DB=/path/copy.db   required; probe aborts when unset
///   X_PROBE_ITEMS=50                entities sampled per task (≤100/task)
///   X_PROBE_MODE=publish|nochange   publish = new version per item;
///                                   nochange = reuse bound to stored head
///   X_PROBE_AVAIL=1                 also time Observer::availability over
///                                   100-entity batches (driver pre-scan)
#[test]
#[ignore = "manual real-DB probe; needs X_DATA_PROBE_DB=<copy>"]
fn yearly_real_data_probe() {
    use crate::market_update::tasks::Operation;
    use crate::sqlite_market::Publisher;
    use crate::task_control::{Contract, RequestOutcome};
    use crate::yearly_publication::{
        Derivation, DerivationEvidence,
        results::YearlyRepository,
        runner::{YearlyRunner, YearlyVerifier},
        tasks::YearlyContract,
    };
    use chrono::Datelike;
    use std::path::Path;
    use std::time::Instant;

    // Mirrors runner::bound_evidence — same coverage checks and the exact
    // stored evidence wording so `nochange` derivations reproduce the
    // stored record bit-for-bit and exercise the reuse path.
    struct Stub;
    impl YearlyVerifier for Stub {
        fn verify(
            &self,
            base: &CheckedRecord,
            years: &BTreeSet<i32>,
            annual: &crate::yearly::AnnualSeries,
            derived_at: DateTime<Utc>,
        ) -> Result<DerivationEvidence, crate::market_data::SourceError> {
            let as_of = derived_at
                .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                .date_naive();
            if annual.base_daily_version_id != base.version().version_id
                || annual.as_of != as_of
                || annual.records.len() != years.len()
                || !annual
                    .records
                    .iter()
                    .map(|r| r.bar.date.year())
                    .eq(years.iter().copied())
            {
                return Err(crate::market_data::SourceError::NotVerified(
                    "aggregated annual series does not cover the requested years".into(),
                ));
            }
            let generation = std::env::var("X_PROBE_GEN").unwrap_or_else(|_| {
                "804c5a63b5283a4cc02ce582166dde25cbb76057e54a54a2910bc4bba8b07d0b".into()
            });
            Ok(DerivationEvidence {
                base_integrity_sha256: base.raw()["integrity_sha256"].as_str().unwrap().into(),
                years: years.clone(),
                as_of,
                coverage: CoverageEvidence {
                    status: crate::market_data::CoverageStatus::Complete,
                    reason: format!(
                        "selected years verified by re-aggregating fixed daily {} against bound generation {generation} as of {as_of}; not a listing-history claim",
                        base.version().version_id
                    ),
                },
            })
        }
    }

    let Ok(db) = std::env::var("X_DATA_PROBE_DB") else {
        eprintln!("skipped: X_DATA_PROBE_DB unset (point at a DB *copy*, never the live path)");
        return;
    };
    let n_items: usize = std::env::var("X_PROBE_ITEMS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let nochange = std::env::var("X_PROBE_MODE").ok().as_deref() == Some("nochange");
    let avail = std::env::var("X_PROBE_AVAIL").ok().as_deref() == Some("1");
    let repo = SqliteCompactMarket::open(Path::new(&db)).unwrap();

    // entity -> (daily head version_id, yearly head version_id, yearly as_of_day)
    let entities: Vec<(String, String, Option<String>, Option<i64>)> = repo
        .read(|c| {
            let mut s = c
                .prepare(
                    "SELECT v.entity, v.version_id,
                            (SELECT v2.version_id FROM effective_heads h2
                             JOIN data_versions v2 ON v2.version_key = h2.version_key
                             WHERE h2.entity = v.entity AND h2.period = 2 AND h2.adjustment = 1),
                            (SELECT l.as_of_day FROM effective_heads h2
                             JOIN yearly_lineage l ON l.version_key = h2.version_key
                             WHERE h2.entity = v.entity AND h2.period = 2 AND h2.adjustment = 1)
                     FROM effective_heads h
                     JOIN data_versions v ON v.version_key = h.version_key
                     WHERE h.period = 1 AND h.adjustment = 1
                     ORDER BY v.entity",
                )
                .map_err(storage)?;
            s.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)
        })
        .unwrap();
    eprintln!("real-db entities={}", entities.len());

    // Keep entities whose stored daily base covers the whole 2025-2026
    // window — the years set the preview.24 driver froze into every item.
    let (first_day, last_day) = (
        compact_market::epoch_day(NaiveDate::from_ymd_opt(2025, 1, 1).unwrap()),
        compact_market::epoch_day(NaiveDate::from_ymd_opt(2026, 12, 31).unwrap()),
    );
    let usable: Vec<_> = repo
        .read(|c| {
            Ok::<_, Error>(
                entities
                    .iter()
                    .filter(|(_, daily, _, _)| {
                        crate::sqlite_market::compact_versions::window_years(
                            c, daily, first_day, last_day,
                        )
                        .map(|years| years.is_superset(&BTreeSet::from([2025, 2026])))
                        .unwrap_or(false)
                    })
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .unwrap();
    eprintln!("usable(2025+2026)={}", usable.len());

    if avail {
        use crate::data_supply::{
            Access,
            observe::Observer,
            wire::{ContractVersion, Dataset, Filter, Query, Selection, VersionChoice},
        };
        let observer = Observer::new(
            crate::sqlite_supply::SqliteSupply {
                market: crate::sqlite_market::backend::MarketBackend::Compact(repo.clone()),
            },
            "probe-run".into(),
        );
        let access = Access::local();
        let securities = |chunk: &[(String, String, Option<String>, Option<i64>)]| {
            chunk
                .iter()
                .map(|(symbol, ..)| {
                    let (exchange, code) = symbol.split_once('.').unwrap();
                    serde_json::from_value::<crate::data_supply::wire::Security>(
                        json!({"exchange":exchange,"code":code}),
                    )
                    .unwrap()
                })
                .collect::<Vec<_>>()
        };
        // X_PROBE_AVAIL_DUMP=<file> walks every batch and writes the
        // concatenated items JSON — deterministic given a fixed run_id, so
        // identical bytes across implementations prove wire equivalence.
        let dump = std::env::var("X_PROBE_AVAIL_DUMP").ok();
        let mut dumped = Vec::new();
        let chunks: Vec<_> = usable.chunks(100).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            if dump.is_none() && i >= 3 {
                break;
            }
            let query = Query {
                dataset: Dataset::Daily,
                contract_version: ContractVersion::V1,
                selection: Selection::Batch {
                    entities: securities(chunk),
                },
                filter: Filter::Daily {
                    start_date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
                    end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
                },
                version: VersionChoice::LatestPerEntity,
                page_size: 1000,
                cursor: None,
            };
            let t = Instant::now();
            let page = observer.availability(query, &access).unwrap();
            eprintln!(
                "availability batch={} entities={} ms={:.1} (per_entity={:.2}ms)",
                i,
                chunk.len(),
                t.elapsed().as_secs_f64() * 1000.0,
                t.elapsed().as_secs_f64() * 1000.0 / chunk.len() as f64,
            );
            if dump.is_some() {
                dumped.extend(page.items.iter().map(|i| serde_json::to_value(i).unwrap()));
            }
        }
        if let Some(path) = dump {
            std::fs::write(&path, serde_json::to_vec(&dumped).unwrap()).unwrap();
            eprintln!("availability dump {} items -> {path}", dumped.len());
        }
        if std::env::var("X_PROBE_AVAIL_PARTS").ok().as_deref() == Some("1") {
            // Component split of SqliteSupply::availability: task_facts scan
            // + per-entity clone, query() per entity, open-gap read.
            use crate::data_supply::wire::Dataset as D;
            repo.read(|c| {
                let t = Instant::now();
                let tasks = crate::sqlite_supply::task_list(c, &access).unwrap();
                let task_list_ms = t.elapsed().as_secs_f64() * 1000.0;
                let mut scan_ms = 0.0;
                let mut clone_ms = 0.0;
                let mut query_ms = 0.0;
                let mut gaps_ms = 0.0;
                let probe: Vec<String> =
                    usable.iter().take(20).map(|e| e.0.clone()).collect();
                for symbol in &probe {
                    let e = securities(&[(symbol.clone(), String::new(), None, None)])
                        .into_iter()
                        .next()
                        .unwrap();
                    let t = Instant::now();
                    let hits: Vec<_> = tasks
                        .iter()
                        .filter(|f| {
                            f.snapshot
                                .items
                                .iter()
                                .any(|i| i.spec.target["symbol"] == e.symbol())
                        })
                        .collect();
                    scan_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    let _cloned: Vec<_> = hits.iter().map(|f| (*f).clone()).collect();
                    clone_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let single = crate::data_supply::query::QueryPlan::new(
                        Query {
                            dataset: D::Daily,
                            contract_version: crate::data_supply::wire::ContractVersion::V1,
                            selection: Selection::Single {
                                entity: e.clone(),
                            },
                            filter: Filter::Daily {
                                start_date: NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(),
                                end_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
                            },
                            version: VersionChoice::LatestPerEntity,
                            page_size: 1000,
                            cursor: None,
                        },
                        &access,
                    )
                    .unwrap();
                    let t = Instant::now();
                    let _s = crate::sqlite_supply::query(
                        c,
                        &mut crate::sqlite_market::compact_versions::VerifyCtx::default(),
                        &single,
                        &access,
                    )
                    .unwrap();
                    query_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    let _g = crate::sqlite_market::gaps::read_open_gaps(
                        c,
                        &e.symbol(),
                        crate::market_data::Period::Day,
                    )
                    .unwrap();
                    gaps_ms += t.elapsed().as_secs_f64() * 1000.0;
                }
                eprintln!(
                    "avail-parts n={} task_list={:.1}ms scan={:.2}ms/entity clone={:.2}ms/entity query={:.2}ms/entity gaps={:.2}ms/entity",
                    probe.len(),
                    task_list_ms,
                    scan_ms / probe.len() as f64,
                    clone_ms / probe.len() as f64,
                    query_ms / probe.len() as f64,
                    gaps_ms / probe.len() as f64,
                );
                Ok(())
            })
            .unwrap();
        }
    }

    // Yearly task items mirroring the preview.24 frozen shape.
    let item_value =
        |(symbol, daily, yearly, as_of): &(String, String, Option<String>, Option<i64>)| {
            let (intent, comparison) = if nochange {
                (
                    json!({"CurrentHead":{
                        "expected_daily_head":daily,
                        "expected_yearly_head":yearly,
                    }}),
                    yearly.clone(),
                )
            } else {
                (json!("HistoricalFixedBase"), None)
            };
            (
                json!({
                    "symbol":symbol,
                    "base_daily_version_id":daily,
                    "years":[2025,2026],
                    "intent":intent,
                    "comparison_version_id":comparison,
                }),
                *as_of,
            )
        };

    const STAGES: [&str; 8] = [
        "binding+receipt",
        "claim+start_item",
        "fixed_window",
        "aggregate",
        "verify",
        "task_recheck",
        "prepare_resid",
        "commit(+item close)",
    ];
    let mut timings: [Vec<f64>; 8] = Default::default();
    let mut totals: [Vec<f64>; 9] = Default::default();
    let (mut accept_ms, mut resolve_ms) = (vec![], vec![]);
    let mut run_item_ms = vec![];
    let mut no_change_count = 0usize;
    let mut publish_count = 0usize;

    // Task A (items 0..n): manual stage replication — per-stage detail.
    // Task B (items n..2n): production runner.run_item — honest totals.
    let task_chunks: Vec<Vec<_>> = usable
        .chunks(100)
        .take(2)
        .map(|c| c.iter().take(n_items).collect())
        .collect();
    for (task_ix, chunk) in task_chunks.iter().enumerate() {
        if chunk.is_empty() {
            break;
        }
        let items: Vec<_> = chunk.iter().map(|e| item_value(e)).collect();
        let contract = YearlyContract {
            operation: Operation::Update,
            repository: repo.clone(),
            today: || NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        };
        let t = Instant::now();
        let work = contract
            .resolve(&json!({"items":items.iter().map(|(v,_)| v.clone()).collect::<Vec<_>>()}))
            .unwrap();
        resolve_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let command = Command {
            key: contract.key(),
            intent: json!({}),
        };
        let t = Instant::now();
        let acceptance = repo
            .accept(
                &format!("probe-req-{task_ix}-{}", std::process::id()),
                &command,
                &work,
                RUN,
                None,
            )
            .unwrap();
        accept_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let RequestOutcome::Task(snapshot) = acceptance.record.outcome else {
            panic!("expected task outcome")
        };
        let yearly_task = snapshot.task.task_id.clone();

        for ((_item_value_json, as_of_day), item) in items.iter().zip(snapshot.items.iter()) {
            let item_id = item.spec.item_id.clone();
            // derived_at pinned to the entity's stored as_of so `nochange`
            // derivations land identical content and take the reuse path.
            let derived_at = match as_of_day {
                Some(day) => compact_market::epoch_date(*day)
                    .unwrap()
                    .and_hms_opt(4, 0, 0)
                    .unwrap()
                    .and_utc(),
                None => ts(),
            };
            if task_ix == 1 {
                let runner = YearlyRunner {
                    repository: repo.clone(),
                    run_id: RUN.into(),
                    derived_at,
                };
                let t = Instant::now();
                let out = runner.run_item(&yearly_task, &item_id, &Stub).unwrap();
                run_item_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                assert!(matches!(
                    out,
                    crate::yearly_publication::runner::YearlyExecution::Published(_)
                ));
                continue;
            }

            let t = Instant::now();
            let mut binding = repo
                .task_item_binding(&yearly_task, &item_id)
                .unwrap()
                .unwrap();
            assert!(
                repo.yearly_item_receipt(&yearly_task, &item_id)
                    .unwrap()
                    .is_none()
            );
            timings[0].push(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            if binding.head.status == crate::task_control::Status::Pending {
                repo.transition_task_head(&mut binding.head, None, RUN, Change::Claim)
                    .unwrap();
            }
            repo.transition_task_head(
                &mut binding.head,
                Some(&mut binding.item),
                RUN,
                Change::StartItem(item_id.clone()),
            )
            .unwrap();
            timings[1].push(t.elapsed().as_secs_f64() * 1000.0);

            let frozen = &binding.item.spec.frozen_inputs;
            let base_id = frozen["base_daily_version_id"].as_str().unwrap();
            let years: BTreeSet<i32> = serde_json::from_value(frozen["years"].clone()).unwrap();
            let (first_day, last_day) = (
                NaiveDate::from_ymd_opt(*years.first().unwrap(), 1, 1).unwrap(),
                NaiveDate::from_ymd_opt(*years.last().unwrap(), 12, 31).unwrap(),
            );

            let t = Instant::now();
            let base = repo.fixed_window(base_id, first_day, last_day).unwrap();
            timings[2].push(t.elapsed().as_secs_f64() * 1000.0);

            let as_of = derived_at
                .with_timezone(&chrono::FixedOffset::east_opt(8 * 3600).unwrap())
                .date_naive();
            let t = Instant::now();
            let annual = crate::yearly::aggregate(base.version(), Some(&years), as_of).unwrap();
            timings[3].push(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            let evidence = Stub.verify(&base, &years, &annual, derived_at).unwrap();
            timings[4].push(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            let _recheck = repo.task_head(&yearly_task).unwrap();
            timings[5].push(t.elapsed().as_secs_f64() * 1000.0);

            let id = crate::hash::sha256(json!([yearly_task, item_id]).to_string().as_bytes());
            let request = Derivation {
                symbol: base.version().symbol.clone(),
                base_daily_version_id: base_id.into(),
                version_id: format!("yearly-version-{id}"),
                years: years.clone(),
                derived_at,
            };
            let intent: PublishIntent = serde_json::from_value(frozen["intent"].clone()).unwrap();
            let publisher = Publisher {
                repository: repo.clone(),
            };
            let prior = frozen["comparison"]["version_id"].as_str();

            let t = Instant::now();
            let cand = publisher
                .derive_yearly(&request, &evidence, &base, &annual)
                .unwrap();
            let derive_ms = t.elapsed().as_secs_f64() * 1000.0;
            totals[0].push(derive_ms);

            let t = Instant::now();
            let _b = repo.task_item_binding(&yearly_task, &item_id).unwrap();
            totals[1].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let _f = repo.fixed_facts(base_id).unwrap();
            totals[2].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let f0 = _f.first_day;
            let _w = repo.fixed_window(base_id, f0, f0).unwrap();
            totals[3].push(t.elapsed().as_secs_f64() * 1000.0);
            let vid = request.version_id.clone();
            let t = Instant::now();
            assert!(matches!(repo.candidate(&vid), Err(Error::NotFound)));
            totals[4].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            repo.stage(cand.raw()).unwrap();
            totals[5].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let staged = repo.candidate(&vid).unwrap();
            totals[6].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            repo.mark_validated(&staged).unwrap();
            totals[7].push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let _pack = crate::sqlite_market::envelope::pack(cand.raw()).unwrap();
            totals[8].push(t.elapsed().as_secs_f64() * 1000.0);

            let t = Instant::now();
            let plan = publisher
                .prepare_yearly_result_prefetched(
                    Publication {
                        publication_id: format!("yearly-result-{id}"),
                        version_id: request.version_id.clone(),
                        task_id: binding.head.task_id.clone(),
                        item_id: item_id.clone(),
                        run_id: RUN.into(),
                        expected_revision: binding.head.state_revision,
                        intent,
                    },
                    &cand,
                    prior,
                    Some(&binding),
                    Some(&base),
                )
                .unwrap();
            timings[6].push(
                t.elapsed().as_secs_f64() * 1000.0
                    - derive_ms
                    - totals[1].last().unwrap()
                    - totals[2].last().unwrap()
                    - totals[3].last().unwrap(),
            );

            let t = Instant::now();
            let receipt = repo.commit_yearly(&plan).unwrap();
            timings[7].push(t.elapsed().as_secs_f64() * 1000.0);
            if plan.is_no_change() {
                no_change_count += 1;
            } else {
                publish_count += 1;
                if nochange && publish_count == 1 {
                    let pr = plan.prior.as_ref().unwrap();
                    eprintln!(
                        "NOCHANGE-DIFF prior={} cand={}",
                        serde_json::to_string(pr.raw()).unwrap(),
                        serde_json::to_string(cand.raw()).unwrap(),
                    );
                }
            }
            let _ = receipt;
        }
    }

    let p = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        if v.is_empty() {
            return "-/-/-".to_string();
        }
        format!(
            "{:.3}/{:.3}/{:.3}",
            v[v.len() / 2],
            v[(v.len() * 9) / 10],
            *v.last().unwrap()
        )
    };
    eprintln!(
        "real-db mode={} items_manual={} items_runner={} no_change={} published={}",
        if nochange { "nochange" } else { "publish" },
        timings[0].len(),
        run_item_ms.len(),
        no_change_count,
        publish_count,
    );
    eprintln!("  resolve_p50/p90/max_ms={}", p(&mut resolve_ms));
    eprintln!("  accept_p50/p90/max_ms={}", p(&mut accept_ms));
    for (i, name) in STAGES.iter().enumerate() {
        eprintln!("  {name:>18}: ms p50/p90/max={}", p(&mut timings[i]));
    }
    for (name, i) in [
        ("derive_yearly", 0),
        ("task_item_binding", 1),
        ("fixed_facts", 2),
        ("fixed_window_1d", 3),
        ("candidate_miss", 4),
        ("stage", 5),
        ("candidate_hit", 6),
        ("mark_validated", 7),
        ("envelope_pack", 8),
    ] {
        eprintln!("  {name:>18}: ms p50/p90/max={}", p(&mut totals[i]));
    }
    eprintln!("  run_item_total: ms p50/p90/max={}", p(&mut run_item_ms));
}
/// Yearly `no_change` reuse on compact storage (#281): the stored prior
/// carries only typed facts — the slim envelope omits the derived
/// `local_yearly_derivation_v1` block — yet a re-derivation with the same
/// bound base/years and same-year `as_of` must still reuse the published
/// version instead of appending a duplicate.
#[test]
fn yearly_no_change_reuses_version_on_compact_schema() {
    use crate::market_update::tasks::Operation;
    use crate::task_control::{Contract, RequestOutcome};
    use crate::yearly_publication::{
        DerivationEvidence,
        runner::{YearlyExecution, YearlyRunner, YearlyVerifier},
        tasks::YearlyContract,
    };

    struct Stub(&'static str);
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
                coverage: CoverageEvidence::complete(self.0),
            })
        }
    }

    let dir = tempdir();
    let (_, repo) = repo(&dir);
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

    let yearly_task = |req: &str, head: Value, years: &[i32]| {
        let contract = YearlyContract {
            operation: Operation::Update,
            repository: repo.clone(),
            today: || NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        };
        let work = contract
            .resolve(&json!({"items":[{
                "symbol":"sh.600006",
                "base_daily_version_id":"v1",
                "years":years,
                "intent":head,
                "comparison_version_id":null
            }]}))
            .unwrap();
        let command = Command {
            key: contract.key(),
            intent: json!({}),
        };
        let acceptance = repo.accept(req, &command, &work, RUN, None).unwrap();
        let RequestOutcome::Task(snapshot) = acceptance.record.outcome else {
            panic!("expected task outcome")
        };
        (
            snapshot.task.task_id.clone(),
            snapshot.items[0].spec.item_id.clone(),
        )
    };
    let publish =
        |label: &str, task: &(String, String), derived_at: DateTime<Utc>, reason: &'static str| {
            let runner = YearlyRunner {
                repository: repo.clone(),
                run_id: RUN.into(),
                derived_at,
            };
            let YearlyExecution::Published(receipt) =
                runner.run_item(&task.0, &task.1, &Stub(reason)).unwrap()
            else {
                let snap = repo.task(&task.0).unwrap();
                panic!("{label} did not publish: {:?}", snap.items[0].outcome)
            };
            receipt
        };
    let intent = |yearly_head: Option<&str>| json!({"CurrentHead":{"expected_daily_head":"v1","expected_yearly_head":yearly_head}});

    // First derivation publishes a new yearly version.
    let first = publish(
        "first",
        &yearly_task("req-yearly-a", intent(None), &[2025, 2026]),
        ts(),
        "stub re-aggregation verified",
    );
    // A same-year re-derivation bound to that head reuses it — the stored
    // slim record carries every compared typed fact even though it omits
    // the derived envelope block.
    let second = publish(
        "second",
        &yearly_task(
            "req-yearly-b",
            intent(Some(&first.version_id)),
            &[2025, 2026],
        ),
        ts() + chrono::Duration::days(1),
        "stub re-aggregation verified",
    );
    assert_eq!(second.version_id, first.version_id);

    // A yearly derivation over a different daily base is not equivalent to
    // the reused head: binding it to that head must publish a new version.
    let days2 = [
        NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(),
        NaiveDate::from_ymd_opt(2025, 6, 30).unwrap(),
        NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
        NaiveDate::from_ymd_opt(2026, 9, 18).unwrap(),
    ]
    .into_iter()
    .map(compact_market::epoch_day);
    let version2 = mk_version(
        "v2",
        days2
            .clone()
            .enumerate()
            .map(|(i, d)| bar(d, 100 + i as i64))
            .chain(std::iter::once(bar(
                compact_market::epoch_day(NaiveDate::from_ymd_opt(2026, 9, 21).unwrap()),
                150,
            )))
            .collect(),
    );
    repo.stage(&envelope(&version2, source("gen-1"), Some("v1")))
        .unwrap();
    let candidate2 = repo.candidate("v2").unwrap();
    repo.mark_validated(&candidate2).unwrap();
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
                operation: "collect_data".into(),
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
    repo.commit(&PublishPlan {
        command: Publication {
            expected_revision: revision2,
            intent: PublishIntent::Daily {
                expected_head: Some("v1".into()),
            },
            ..collect_command(&task_id2, "pub-2", "v2", revision2)
        },
        record: candidate2.clone(),
        required_heads: vec![(Scope::of(&candidate2.version), Some("v1".into()))],
        promote: None,
        base_digest: None,
    })
    .unwrap();
    let yearly_task_v2 = |req: &str, head: Value| {
        let contract = YearlyContract {
            operation: Operation::Update,
            repository: repo.clone(),
            today: || NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        };
        let work = contract
            .resolve(&json!({"items":[{
                "symbol":"sh.600006",
                "base_daily_version_id":"v2",
                "years":[2025,2026],
                "intent":head,
                "comparison_version_id":null
            }]}))
            .unwrap();
        let command = Command {
            key: contract.key(),
            intent: json!({}),
        };
        let acceptance = repo.accept(req, &command, &work, RUN, None).unwrap();
        let RequestOutcome::Task(snapshot) = acceptance.record.outcome else {
            panic!("expected task outcome")
        };
        (
            snapshot.task.task_id.clone(),
            snapshot.items[0].spec.item_id.clone(),
        )
    };
    let third = publish(
        "third",
        &yearly_task_v2(
            "req-yearly-c",
            json!({"CurrentHead":{"expected_daily_head":"v2","expected_yearly_head":first.version_id}}),
        ),
        ts(),
        "stub re-aggregation verified",
    );
    assert_ne!(third.version_id, first.version_id);
}

/// XP-009 read-path equivalence: the header-only slim projection must expose
/// a byte-identical `raw()` envelope and identical header `version()` fields
/// to the full verified materialization, for checkpoint and delta leaves
/// alike; non-published and missing rows must fall back exactly.
#[test]
fn slim_published_read_matches_full_materialization() {
    use crate::market_update::{UpdateBatchItem, UpdateBatchPrepared};

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
    repo.stage(&envelope(&version, source("gen-1"), None))
        .unwrap();
    let candidate = repo.candidate("v1").unwrap();
    repo.mark_validated(&candidate).unwrap();
    repo.read(|c| {
        // Validated (unpublished) rows never take the slim projection.
        let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
        assert!(materialize_slim(c, &mut ctx, "v1").unwrap().is_none());
        assert!(materialize_slim(c, &mut ctx, "absent").unwrap().is_none());
        assert!(matches!(
            crate::sqlite_market::read_record_slim(c, &mut ctx, "absent", true, true),
            Err(Error::NotFound)
        ));
        Ok::<_, Error>(())
    })
    .unwrap();
    repo.commit(&PublishPlan {
        command: collect_command(&task_id, "pub-1", "v1", revision),
        record: candidate.clone(),
        required_heads: vec![(Scope::of(&candidate.version), None)],
        promote: None,
        base_digest: None,
    })
    .unwrap();

    // A second, delta-linked version (`commit_update_batch` Append).
    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "bars".into(),
            frozen_inputs: json!({}),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope.clone()]),
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
    let update_task = match repo
        .accept("req-upd", &command, &work, RUN, None)
        .unwrap()
        .record
        .outcome
    {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    repo.commit_update_batch(
        &update_task,
        &[UpdateBatchItem {
            task_id: update_task.clone(),
            item_id: ITEM.into(),
            publication_id: "upd-1".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: scope,
            prepared: UpdateBatchPrepared::Append {
                base_version_id: "v1".into(),
                result_version_id: "v2".into(),
                upserts: vec![bar(3, 130)],
                metadata: None,
                continuity: json!({
                    "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}
                }),
            },
        }],
    )
    .unwrap();

    repo.read(|c| {
        let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
        for id in ["v1", "v2"] {
            let full = materialize_checked(c, id, false).unwrap();
            let slim = materialize_slim(c, &mut ctx, id).unwrap().unwrap();
            // The published envelope is slim either way: byte-identical.
            assert_eq!(slim.raw(), full.raw(), "raw() differs for {id}");
            assert_eq!(slim.version().version_id, full.version().version_id);
            assert_eq!(slim.version().symbol, full.version().symbol);
            assert_eq!(slim.version().period, full.version().period);
            assert_eq!(slim.version().source, full.version().source);
            assert_eq!(slim.version().published_at, full.version().published_at);
            assert_eq!(
                slim.version().validation.record_count,
                full.version().validation.record_count
            );
            assert!(slim.version().records.is_empty());
            assert!(!full.version().records.is_empty());
            // materialize_published resolves to the same record either way.
            let via = materialize_published(c, &mut ctx, id).unwrap();
            assert_eq!(via.raw(), full.raw());
        }
        Ok::<_, Error>(())
    })
    .unwrap();
}

/// XP-009: `read_bound_task_light` must project the same summary/authorized/
/// symbol surface as `read_bound_task` on the same bound task.
#[test]
fn light_bound_task_matches_full_projection() {
    use crate::data_supply::{Access, observe};

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    repo.read(|c| {
        let full = crate::sqlite_tasks::read_bound_task(c, &task_id)
            .unwrap()
            .unwrap();
        let light = crate::sqlite_tasks::read_bound_task_light(c, &task_id)
            .unwrap()
            .unwrap();
        // Everything the related-task/summary surface consumes is identical.
        assert_eq!(light.task, full.task);
        let symbols = |s: &crate::task_control::TaskSnapshot| {
            s.items
                .iter()
                .map(|i| {
                    (
                        i.spec.item_id.clone(),
                        i.spec.target["symbol"].as_str().map(str::to_owned),
                        i.spec.target["period"].as_str().map(str::to_owned),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(symbols(&light), symbols(&full));
        let access = Access::local();
        assert_eq!(
            observe::authorized(&light, &access),
            observe::authorized(&full, &access)
        );
        assert_eq!(light.visible_status("probe"), full.visible_status("probe"));
        // Placeholders stay unambiguous for misuse detection.
        assert!(light.claims.is_empty());
        assert!(light.items.iter().all(|i| i.outcome.is_none()));
        assert!(
            crate::sqlite_tasks::read_bound_task_light(c, "absent")
                .unwrap()
                .is_none()
        );
        Ok::<_, Error>(())
    })
    .unwrap();
}

/// XP-009 end-to-end on compact storage: progress facts and availability
/// answers over published + update-result rows keep the exact reference and
/// count/date surface while read paths no longer materialize records.
#[test]
fn compact_progress_and_availability_keep_reference_surface() {
    use crate::data_supply::{
        Access,
        observe::{ObservationRepository, Observer},
        query::ReadRepository,
        wire::{
            ContractVersion, Dataset, Exchange, Filter, ProgressQuery, Query, Security, Selection,
            VersionChoice,
        },
    };
    use crate::market_update::{UpdateBatchItem, UpdateBatchPrepared};
    use crate::sqlite_market::backend::MarketBackend;
    use crate::sqlite_supply::SqliteSupply;

    let dir = tempdir();
    let (_, repo) = repo(&dir);
    let task_id = accept_task(&repo);
    let revision = start_item(&repo, &task_id);
    let version = mk_version("v1", vec![bar(0, 100), bar(1, 110), bar(2, 120)]);
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

    let scope = task_control::legacy::scope_id("sh.600006", "day");
    let work = FrozenWork {
        items: vec![ItemSpec {
            item_id: ITEM.into(),
            target: json!({"symbol": "sh.600006", "period": "day"}),
            progress_unit: "security_period".into(),
            frozen_inputs: crate::market_update::tasks::frozen(
                "v1",
                "ab".repeat(32).as_str(),
                &crate::market_update::DateRange {
                    start: date(0),
                    end: date(3),
                },
            ),
            scope_keys: BTreeSet::from([scope.clone()]),
        }],
        scopes: BTreeSet::from([scope.clone()]),
        shared_evidence: None,
    };
    let command = Command {
        key: CommandKey {
            operation: "update_data".into(),
            dataset: "market.daily".into(),
            contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
        },
        intent: json!({}),
    };
    let update_task = match repo
        .accept("req-upd", &command, &work, RUN, None)
        .unwrap()
        .record
        .outcome
    {
        task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
        _ => panic!("expected task outcome"),
    };
    repo.commit_update_batch(
        &update_task,
        &[UpdateBatchItem {
            task_id: update_task.clone(),
            item_id: ITEM.into(),
            publication_id: "upd-1".into(),
            run_id: RUN.into(),
            symbol: "sh.600006".into(),
            occupancy_key: scope,
            prepared: UpdateBatchPrepared::Append {
                base_version_id: "v1".into(),
                result_version_id: "v2".into(),
                upserts: vec![bar(3, 130)],
                metadata: None,
                continuity: json!({
                    "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}
                }),
            },
        }],
    )
    .unwrap();

    let supply = SqliteSupply {
        market: MarketBackend::Compact(repo.clone()),
    };
    let access = Access::local();
    let entity = Security {
        exchange: Exchange::Sh,
        code: "600006".into(),
    };

    // Progress facts resolve real references (not placeholder digests).
    let facts = supply.task(&update_task, &access).unwrap();
    assert_eq!(facts.request_id, "req-upd");
    assert_eq!(facts.range_results.len(), 1);
    let digest_of = |id: &str| {
        repo.read(|c| {
            Ok::<_, Error>(
                materialize_checked(c, id, false).unwrap().raw()["integrity_sha256"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            )
        })
        .unwrap()
    };
    let v1_digest = digest_of("v1");
    let v2_digest = digest_of("v2");
    let rr = &facts.range_results[0];
    assert_eq!(rr.base_version.version_id, "v1");
    assert_eq!(rr.base_version.integrity_sha256, v1_digest);
    assert_eq!(rr.result_version.version_id, "v2");
    assert_eq!(rr.result_version.integrity_sha256, v2_digest);
    assert_eq!(rr.item_id, ITEM);
    let publications = &facts.publications;
    assert_eq!(publications.len(), 1);
    assert_eq!(publications[0].version_id, "v2");

    // The observer page round-trips the same facts and verifies them.
    let observer = Observer::new(supply.clone(), "probe-run".into());
    let page = observer
        .progress(
            ProgressQuery {
                task_id: update_task.clone(),
                page_size: 100,
                cursor: None,
            },
            &access,
        )
        .unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.succeeded, 1);
    assert_eq!(page.range_results.len(), 1);
    assert_eq!(page.publication_versions[0].version_id, "v2");

    // Availability over a partial window counts only in-range bars.
    let availability = |start: NaiveDate, end: NaiveDate| {
        observer
            .availability(
                Query {
                    dataset: Dataset::Daily,
                    contract_version: ContractVersion::V1,
                    selection: Selection::Single {
                        entity: entity.clone(),
                    },
                    filter: Filter::Daily {
                        start_date: start,
                        end_date: end,
                    },
                    version: VersionChoice::LatestPerEntity,
                    page_size: 10,
                    cursor: None,
                },
                &access,
            )
            .unwrap()
    };
    let page = availability(date(0), date(3));
    assert_eq!(page.items.len(), 1);
    let item = &page.items[0];
    assert_eq!(item.record_count, 4);
    assert_eq!(item.first_record, Some(date(0)));
    assert_eq!(item.last_record, Some(date(3)));
    assert_eq!(item.version_ref.as_ref().unwrap().version_id, "v2");
    assert_eq!(
        item.version_ref.as_ref().unwrap().integrity_sha256,
        v2_digest
    );
    // Both tasks relate to the symbol; summaries carry real fields.
    let related: Vec<String> = item
        .related_tasks
        .iter()
        .map(|t| t.task_id.clone())
        .collect();
    assert!(related.contains(&update_task) && related.contains(&task_id));
    assert!(item.related_tasks.iter().all(|t| !t.request_id.is_empty()));

    let page = availability(date(1), date(2));
    assert_eq!(page.items[0].record_count, 2);
    assert_eq!(page.items[0].first_record, Some(date(1)));
    assert_eq!(page.items[0].last_record, Some(date(2)));

    // verify_versions still passes on the slim-derived references.
    supply.verify(&[item.version_ref.clone().unwrap()]).unwrap();
}

/// XP-011 Phase-2 corruption regressions (Owner 6+1 matrix): every
/// optimized read surface — slim envelopes plus the transaction-scoped
/// verified-facts context — must expose the same public
/// integrity-failure/unavailable classification as the full verified
/// path on the identical corrupted fixture.
mod xp011 {
    use super::*;
    use crate::data_supply::Access;
    use crate::data_supply::observe::{ObservationRepository, Observer};
    use crate::data_supply::wire::ProgressQuery;
    use crate::market_update::{UpdateBatchItem, UpdateBatchPrepared};
    use crate::sqlite_market::backend::MarketBackend;
    use crate::sqlite_supply::SqliteSupply;

    /// Public outcome classification used for optimized-vs-verified
    /// comparison — the same surface an MCP caller sees.
    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum ReadClass {
        Ok,
        Integrity,
        TaskError,
        NotFound,
        Invalid,
        Other,
    }

    fn read_class<T>(result: &Result<T, Error>) -> ReadClass {
        match result {
            Ok(_) => ReadClass::Ok,
            Err(Error::Integrity(_)) => ReadClass::Integrity,
            Err(Error::Task(_)) => ReadClass::TaskError,
            Err(Error::NotFound) => ReadClass::NotFound,
            Err(Error::Invalid(_)) => ReadClass::Invalid,
            Err(_) => ReadClass::Other,
        }
    }

    /// Published daily heads v1a/v1b for two entities plus a three-item
    /// update task where only item-1 commits an append result v2a — the
    /// two unfinished items make progress paging need a cursor.
    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        repo: SqliteCompactMarket,
        task: String,
        access: Access,
    }

    fn publish_head(
        repo: &SqliteCompactMarket,
        request: &str,
        pub_id: &str,
        symbol: &str,
        version_id: &str,
        bars: Vec<Bar>,
    ) {
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
            .accept(request, &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        };
        let revision = start_item(repo, &task_id);
        let mut version = mk_version(version_id, bars);
        version.symbol = symbol.into();
        repo.stage(&envelope(&version, source("gen-1"), None))
            .unwrap();
        let candidate = repo.candidate(version_id).unwrap();
        repo.mark_validated(&candidate).unwrap();
        repo.commit(&PublishPlan {
            command: collect_command(&task_id, pub_id, version_id, revision),
            record: candidate.clone(),
            required_heads: vec![(Scope::of(&candidate.version), None)],
            promote: None,
            base_digest: None,
        })
        .unwrap();
    }

    fn fixture() -> Fixture {
        let dir = tempdir();
        let (path, repo) = repo(&dir);
        let bars = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
        publish_head(&repo, "req-a", "pub-a", "sh.600006", "v1a", bars.clone());
        publish_head(&repo, "req-b", "pub-b", "sh.600007", "v1b", bars);
        let scope_a = task_control::legacy::scope_id("sh.600006", "day");
        let scope_b = task_control::legacy::scope_id("sh.600007", "day");
        let item = |item_id: &str, symbol: &str, scope: &str, base: &str| ItemSpec {
            item_id: item_id.into(),
            target: json!({"symbol": symbol, "period": "day"}),
            progress_unit: "security_period".into(),
            frozen_inputs: crate::market_update::tasks::frozen(
                base,
                "ab".repeat(32).as_str(),
                &crate::market_update::DateRange {
                    start: date(0),
                    end: date(3),
                },
            ),
            scope_keys: BTreeSet::from([scope.to_string()]),
        };
        let work = FrozenWork {
            items: vec![
                item("item-1", "sh.600006", &scope_a, "v1a"),
                item("item-2", "sh.600007", &scope_b, "v1b"),
                item("item-3", "sh.600006", &scope_a, "v1a"),
            ],
            scopes: BTreeSet::from([scope_a.clone(), scope_b]),
            shared_evidence: None,
        };
        let command = Command {
            key: CommandKey {
                operation: "update_data".into(),
                dataset: "market.daily".into(),
                contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
            },
            intent: json!({}),
        };
        let task = match repo
            .accept("req-upd", &command, &work, RUN, None)
            .unwrap()
            .record
            .outcome
        {
            task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
            _ => panic!("expected task outcome"),
        };
        repo.commit_update_batch(
            &task,
            &[UpdateBatchItem {
                task_id: task.clone(),
                item_id: "item-1".into(),
                publication_id: "upd-1".into(),
                run_id: RUN.into(),
                symbol: "sh.600006".into(),
                occupancy_key: scope_a,
                prepared: UpdateBatchPrepared::Append {
                    base_version_id: "v1a".into(),
                    result_version_id: "v2a".into(),
                    upserts: vec![bar(3, 130)],
                    metadata: None,
                    continuity: json!({
                        "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}
                    }),
                },
            }],
        )
        .unwrap()
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
        Fixture {
            _dir: dir,
            path,
            repo,
            task,
            access: Access::local(),
        }
    }

    /// Mutate the fixture database directly — the corruption channel the
    /// fail-closed reads must detect regardless of read surface.
    fn tamper(path: &PathBuf, sql: &str) {
        let c = rusqlite::Connection::open(path).unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA ignore_check_constraints=ON")
            .unwrap();
        c.execute(sql, []).unwrap();
    }

    fn supply(repo: &SqliteCompactMarket) -> SqliteSupply {
        SqliteSupply {
            market: MarketBackend::Compact(repo.clone()),
        }
    }

    /// Optimized public surface: `task_facts` with its slim envelopes and
    /// verified-facts context.
    fn optimized_task(f: &Fixture) -> Result<crate::data_supply::observe::TaskFacts, Error> {
        supply(&f.repo).task(&f.task, &f.access)
    }

    /// Optimized public surface: fresh `Observer::progress` — runs
    /// `task_facts` plus the fresh-response `verify_*` transactions.
    fn optimized_progress(
        f: &Fixture,
        cursor: Option<String>,
        page_size: usize,
        access: &Access,
    ) -> Result<crate::data_supply::wire::ProgressPage, Error> {
        Observer::new(supply(&f.repo), RUN.into()).progress(
            ProgressQuery {
                task_id: f.task.clone(),
                page_size,
                cursor,
            },
            access,
        )
    }

    /// Full verified path for a published version — unchanged by XP-011.
    fn verified_version(f: &Fixture, id: &str) -> Result<(), Error> {
        f.repo
            .read(|c| materialize_checked(c, id, false).map(|_| ()))
    }

    /// Full bound-task validation — unchanged by XP-011.
    fn verified_task(f: &Fixture) -> Result<(), Error> {
        f.repo
            .read(|c| Ok(crate::sqlite_tasks::read_bound_task(c, &f.task)?))
            .map(|_| ())
    }

    /// (1) A mutated segment bar with the stored inventory digest left
    /// untouched must fail closed on both read surfaces.
    #[test]
    fn segment_bar_tamper_fails_closed_both_paths() {
        let f = fixture();
        assert!(optimized_task(&f).is_ok());
        tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
        let opt = read_class(&optimized_task(&f));
        let progress = read_class(&optimized_progress(&f, None, 100, &f.access));
        let full = read_class(&verified_version(&f, "v2a"));
        assert_eq!(opt, ReadClass::Integrity);
        assert_eq!(progress, opt);
        assert_eq!(full, opt);
    }

    /// (2) Missing/broken clip linkage or projection-digest drift on the
    /// published result must fail closed on both read surfaces.
    #[test]
    fn clip_or_projection_tamper_fails_closed_both_paths() {
        for sql in [
            "DELETE FROM version_delta_clips WHERE version_key=\
             (SELECT version_key FROM data_versions WHERE version_id='v2a')",
            "UPDATE data_versions SET projection_digest=\
             x'0000000000000000000000000000000000000000000000000000000000000000' \
             WHERE version_id='v2a'",
        ] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let full = read_class(&verified_version(&f, "v2a"));
            assert_eq!(opt, ReadClass::Integrity);
            assert_eq!(full, opt);
        }
    }

    /// (3) Task item stage/outcome/counter state mutated while task and
    /// request digests stay valid — the full bound-task checks must still
    /// fire on the optimized surface (read_bound_task_light revert).
    #[test]
    fn task_item_tamper_fails_closed_both_paths() {
        for sql in [
            "UPDATE task_items SET stage='pending' WHERE item_id='item-1'",
            "UPDATE tasks SET item_count=99",
        ] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let progress = read_class(&optimized_progress(&f, None, 100, &f.access));
            let full = read_class(&verified_task(&f));
            assert!(opt != ReadClass::Ok && full != ReadClass::Ok);
            assert_eq!(opt, full);
            assert_eq!(progress, full);
        }
    }

    /// (4) Missing/mismatched task scopes or occupancy claims for the task
    /// state must fail closed on both read surfaces.
    #[test]
    fn scope_occupancy_tamper_fails_closed_both_paths() {
        for sql in ["DELETE FROM scope_occupancy", "DELETE FROM task_scopes"] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let full = read_class(&verified_task(&f));
            assert!(opt != ReadClass::Ok && full != ReadClass::Ok);
            assert_eq!(opt, full);
        }
    }

    /// (5) Corruption committed between two cursor pages must still be
    /// detected — the resumed page re-verifies against current storage.
    #[test]
    fn corruption_between_cursor_pages_still_fails() {
        let f = fixture();
        let observer = Observer::new(supply(&f.repo), RUN.into());
        let page1 = observer
            .progress(
                ProgressQuery {
                    task_id: f.task.clone(),
                    page_size: 1,
                    cursor: None,
                },
                &f.access,
            )
            .unwrap();
        assert_eq!(page1.unfinished.len(), 1);
        let cursor = page1.next_cursor.clone().expect("second page");
        tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
        let page2 = observer.progress(
            ProgressQuery {
                task_id: f.task.clone(),
                page_size: 1,
                cursor: Some(cursor),
            },
            &f.access,
        );
        assert!(page2.is_err());
        assert_eq!(
            read_class(&page2),
            read_class(&optimized_progress(&f, None, 100, &f.access))
        );
    }

    /// (6) Unauthorized access stays denied with no private diagnostics
    /// leaking through the optimized surface.
    #[test]
    fn unauthorized_access_denied_without_leak() {
        let f = fixture();
        let denied = Access {
            principal: "reader".into(),
            datasets: BTreeSet::new(),
            securities: Some(BTreeSet::new()),
        };
        let task_result = supply(&f.repo).task(&f.task, &denied);
        let progress_result = optimized_progress(&f, None, 100, &denied);
        for result in [read_class(&task_result), read_class(&progress_result)] {
            assert_eq!(result, ReadClass::Invalid);
        }
        match supply(&f.repo).task(&f.task, &denied) {
            Err(Error::Invalid(message)) => assert_eq!(message, "forbidden"),
            other => panic!("expected forbidden, got class {:?}", read_class(&other)),
        }
    }

    /// (7) Legacy/unpublished/missing-version fallback behavior is
    /// unchanged: the optimized surfaces classify identical to the full
    /// readers on the same inputs.
    #[test]
    fn fallback_classifications_unchanged() {
        let f = fixture();
        // Missing version id.
        let slim = read_class(&f.repo.read(|c| {
            let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
            crate::sqlite_market::read_record_slim(c, &mut ctx, "absent", true, true)
        }));
        let full = read_class(
            &f.repo
                .read(|c| crate::sqlite_market::read_record(c, "absent", true, true)),
        );
        assert_eq!(slim, ReadClass::NotFound);
        assert_eq!(slim, full);
        // Validated-but-unpublished version.
        let version = mk_version("v3a", vec![bar(0, 100)]);
        f.repo
            .stage(&envelope(&version, source("gen-1"), None))
            .unwrap();
        let candidate = f.repo.candidate("v3a").unwrap();
        f.repo.mark_validated(&candidate).unwrap();
        let slim = read_class(&f.repo.read(|c| {
            let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
            materialize_published(c, &mut ctx, "v3a")
        }));
        let full = read_class(&f.repo.read(|c| materialize_checked(c, "v3a", false)));
        assert_eq!(slim, full);
        // Missing result row keeps the Ok(None) classification.
        assert!(matches!(
            f.repo
                .read(|c| crate::sqlite_market::update::read_result(c, "absent")),
            Ok(None)
        ));
        // Missing task keeps the task-level not-found classification.
        assert_eq!(
            read_class(&supply(&f.repo).task("absent", &f.access)),
            ReadClass::TaskError
        );
        // A bound-task read of the same absent id stays the plain
        // none-for-missing lookup — unchanged fallback semantics.
        assert!(matches!(
            f.repo
                .read(|c| Ok(crate::sqlite_tasks::read_bound_task(c, "absent")?)),
            Ok(None)
        ));
    }

    /// Same-transaction dedup sanity: a NoChange-style double touch of one
    /// version inside one verified-facts context must reuse the first
    /// verification, not re-run it. Asserted on the measured
    /// `fixed_runs` counter, not only `Rc::ptr_eq`.
    #[test]
    fn verified_ctx_deduplicates_within_transaction() {
        let f = fixture();
        f.repo
            .read(|c| {
                let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                let (facts_a, verified_a) = ctx.verified_by_id(c, "v2a")?;
                let (facts_b, verified_b) = ctx.verified_by_id(c, "v2a")?;
                assert_eq!(facts_a.version_id, facts_b.version_id);
                assert!(std::rc::Rc::ptr_eq(&verified_a, &verified_b));
                assert_eq!(ctx.fixed_runs, 1);
                Ok::<_, Error>(())
            })
            .unwrap();
    }

    /// (2-ext) A missing ancestor version row or a missing referenced
    /// segment must fail closed on both read surfaces — the delta chain
    /// and its clip resolution are part of the public integrity surface.
    #[test]
    fn missing_ancestor_or_segment_fails_closed_both_paths() {
        for sql in [
            // ancestor version row deleted: the v2a delta chain breaks
            "DELETE FROM data_versions WHERE version_id='v1a'",
            // the append result's own delta segment deleted
            "DELETE FROM bar_segments WHERE segment_key IN \
             (SELECT segment_key FROM version_delta_clips WHERE segment_key IS NOT NULL)",
            // the shared checkpoint segment deleted
            "DELETE FROM bar_segments WHERE segment_key IN \
             (SELECT segment_key FROM version_checkpoint_clips)",
        ] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let progress = read_class(&optimized_progress(&f, None, 100, &f.access));
            let full = read_class(&verified_version(&f, "v2a"));
            assert_eq!(opt, ReadClass::Integrity, "sql: {sql}");
            assert_eq!(progress, opt, "sql: {sql}");
            assert_eq!(full, opt, "sql: {sql}");
        }
    }

    /// (3b) An outcome-only mutation — task row and request digests left
    /// valid — must still fail closed on both read surfaces.
    #[test]
    fn outcome_only_tamper_fails_closed_both_paths() {
        for sql in [
            // success flag kept but an error injected into the outcome
            "UPDATE task_items SET outcome_json=\
             json_set(outcome_json,'$.error','\"injected\"') WHERE item_id='item-1'",
            // outcome erased from a finished item (stage/outcome pair)
            "UPDATE task_items SET outcome_json=NULL WHERE item_id='item-1'",
        ] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let progress = read_class(&optimized_progress(&f, None, 100, &f.access));
            let full = read_class(&verified_task(&f));
            assert!(opt != ReadClass::Ok && full != ReadClass::Ok, "sql: {sql}");
            assert_eq!(opt, full, "sql: {sql}");
            assert_eq!(progress, full, "sql: {sql}");
        }
    }

    /// (4b) Mismatched — not just missing — task scopes/occupancy claims
    /// must fail closed on both read surfaces.
    #[test]
    fn scope_occupancy_mismatch_fails_closed_both_paths() {
        for sql in [
            "UPDATE task_scopes SET scope_key=scope_key||':tampered'",
            "UPDATE scope_occupancy SET scope_key=scope_key||':tampered'",
        ] {
            let f = fixture();
            tamper(&f.path, sql);
            let opt = read_class(&optimized_task(&f));
            let full = read_class(&verified_task(&f));
            assert!(opt != ReadClass::Ok && full != ReadClass::Ok, "sql: {sql}");
            assert_eq!(opt, full, "sql: {sql}");
        }
    }

    /// (6b) On a corrupted fixture, unauthorized access is still denied
    /// and the denial carries no private diagnostics — the classification
    /// is exactly "forbidden", not a corruption detail leak.
    #[test]
    fn unauthorized_on_corrupted_fixture_denied_without_leak() {
        let f = fixture();
        tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
        let denied = Access {
            principal: "reader".into(),
            datasets: BTreeSet::new(),
            securities: Some(BTreeSet::new()),
        };
        for result in [
            supply(&f.repo).task(&f.task, &denied).map(|_| ()),
            optimized_progress(&f, None, 100, &denied).map(|_| ()),
        ] {
            match result {
                Err(Error::Invalid(message)) => assert_eq!(message, "forbidden"),
                other => panic!("expected forbidden, got class {:?}", read_class(&other)),
            }
        }
    }

    /// (6c) Same denial through the real MCP boundary: a `tools/call`
    /// against a `DataApi`/`ToolHandler` built on the corrupted fixture
    /// returns exactly `forbidden` over the wire — identical before and
    /// after tampering, with no private diagnostics in code, message,
    /// or details.
    #[test]
    fn unauthorized_on_corrupted_fixture_denied_at_mcp_boundary() {
        use crate::mcp::ToolHandler;
        let f = fixture();
        let denied = Access {
            principal: "reader".into(),
            datasets: BTreeSet::new(),
            securities: Some(BTreeSet::new()),
        };
        // Share the fixture's database lease/backend — a second
        // SqliteCompactMarket::open on the same file would take a
        // second flock.
        let api = crate::data_supply::mcp::DataApi::new_with_backend(
            f.repo.tasks(),
            MarketBackend::Compact(f.repo.clone()),
            RUN.into(),
            denied,
            None,
        )
        .unwrap();
        let assert_denied = |label: &str| {
            for (name, arguments) in [
                (
                    "get_task_progress",
                    json!({"task_id": f.task, "page_size": 100}),
                ),
                ("get_task", json!({"task_id": f.task})),
            ] {
                let error = api.call(name, arguments).unwrap_err();
                assert_eq!(error.code, "forbidden", "{label}: {name}");
                assert!(error.details.is_none(), "{label}: {name}");
                for needle in ["integrity", "digest", "segment", "corrupt"] {
                    assert!(
                        !error.message.to_lowercase().contains(needle),
                        "{label}: {name} leaks diagnostic: {}",
                        error.message
                    );
                }
            }
        };
        assert_denied("healthy");
        tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
        assert_denied("corrupted");
        // Full JSON-RPC round-trip: the serialized wire response must
        // carry the same bare denial.
        let response = crate::mcp::handle_json(
            &api,
            &json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "get_task_progress",
                    "arguments": {"task_id": f.task, "page_size": 100},
                },
            })
            .to_string(),
        )
        .unwrap();
        let wire: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(wire["result"]["isError"], json!(true));
        assert_eq!(
            wire["result"]["structuredContent"]["code"],
            json!("forbidden")
        );
        for needle in ["integrity", "digest", "segment", "corrupt"] {
            assert!(
                !response.to_lowercase().contains(needle),
                "wire response leaks diagnostic: {response}"
            );
        }
    }

    /// (7b) A real legacy-format fixture — a v18 database holding an
    /// actual legacy JSON envelope, not a compact missing/unpublished
    /// case — keeps identical classifications through the slim/ctx entry
    /// points, on a healthy row and after envelope tampering.
    #[test]
    fn legacy_fixture_classification_unchanged() {
        let dir = tempdir();
        let path = dir.path().join("legacy.db");
        let tasks = crate::sqlite_tasks::SqliteTasks::new(path.clone());
        tasks.initialize().unwrap();
        let db = crate::sqlite_market::SqliteMarket::new(tasks);
        let samples: Vec<Value> = serde_json::from_str(include_str!(
            "../../../tests/fixtures/issue79/legacy-records.json"
        ))
        .unwrap();
        db.stage(&samples[0]["record"]).unwrap();
        // Healthy staged legacy record: slim defers to the full legacy
        // read; both surfaces agree.
        let slim = read_class(&db.read(|c| {
            let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
            crate::sqlite_market::read_record_slim(c, &mut ctx, "legacy-daily", false, true)
        }));
        let full = read_class(
            &db.read(|c| crate::sqlite_market::read_record(c, "legacy-daily", false, true)),
        );
        assert_eq!(slim, ReadClass::Ok);
        assert_eq!(slim, full);
        // Published-only request on a staged row: same NotFound both ways.
        let slim = read_class(&db.read(|c| {
            let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
            crate::sqlite_market::read_record_slim(c, &mut ctx, "legacy-daily", true, true)
        }));
        let full = read_class(
            &db.read(|c| crate::sqlite_market::read_record(c, "legacy-daily", true, true)),
        );
        assert_eq!(slim, full);
        // Stored-digest tampering on the legacy store: both fail closed.
        for sql in [
            "UPDATE data_versions SET content_digest='bogus' WHERE version_id='legacy-daily'",
            "UPDATE data_versions SET envelope_json='{\"bogus\":' WHERE version_id='legacy-daily'",
        ] {
            let dir = tempdir();
            let path = dir.path().join("legacy.db");
            let tasks = crate::sqlite_tasks::SqliteTasks::new(path.clone());
            tasks.initialize().unwrap();
            let db = crate::sqlite_market::SqliteMarket::new(tasks);
            db.stage(&samples[0]["record"]).unwrap();
            rusqlite::Connection::open(&path)
                .unwrap()
                .execute(sql, [])
                .unwrap();
            let slim = read_class(&db.read(|c| {
                let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                crate::sqlite_market::read_record_slim(c, &mut ctx, "legacy-daily", false, true)
            }));
            let full = read_class(
                &db.read(|c| crate::sqlite_market::read_record(c, "legacy-daily", false, true)),
            );
            assert_eq!(slim, ReadClass::Integrity, "sql: {sql}");
            assert_eq!(slim, full, "sql: {sql}");
        }
    }

    /// Round-2 R1: the facts memo is bounded at 512 FIFO entries. Past
    /// the bound the oldest entry is evicted and recomputed on revisit —
    /// correctness is preserved and the returned facts equal the uncached
    /// `result_facts` output for every key.
    #[test]
    fn facts_memo_bounded_fifo_revisit_and_equivalence() {
        let f = fixture();
        for i in 0..520 {
            let v = mk_version(&format!("vx{i:04}"), vec![bar(0, 100)]);
            f.repo.stage(&envelope(&v, source("gen-1"), None)).unwrap();
        }
        f.repo
            .read(|c| {
                let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                let mut keys = Vec::new();
                for i in 0..520 {
                    keys.push(version_key(c, &format!("vx{i:04}"))?);
                }
                for &key in &keys {
                    let cached = ctx.facts(c, key)?;
                    let uncached = crate::sqlite_market::compact_versions::result_facts(c, key)?;
                    assert_eq!(*cached, uncached);
                }
                assert_eq!(ctx.facts_misses, 520);
                // FIFO eviction: the first 8 keys are gone; the newest stay.
                let _ = ctx.facts(c, keys[0])?;
                assert_eq!(ctx.facts_misses, 521);
                let _ = ctx.facts(c, keys[8])?;
                assert_eq!(ctx.facts_misses, 522);
                let _ = ctx.facts(c, keys[519])?;
                assert_eq!(ctx.facts_misses, 522);
                Ok::<_, Error>(())
            })
            .unwrap();
    }

    /// Round-2 R2: the verified LRU admits an evicted version to be
    /// rebuilt inside the same transaction — measured on `fixed_runs`,
    /// so the "reuse" guarantee is retention-scoped, not
    /// once-per-transaction.
    #[test]
    fn verified_lru_eviction_reverifies_in_same_transaction() {
        let f = fixture();
        publish_head(
            &f.repo,
            "req-c",
            "pub-c",
            "sh.600010",
            "v1c",
            vec![bar(0, 100)],
        );
        publish_head(
            &f.repo,
            "req-d",
            "pub-d",
            "sh.600011",
            "v1d",
            vec![bar(0, 100)],
        );
        f.repo
            .read(|c| {
                let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                let (_, first) = ctx.verified_by_id(c, "v1a")?;
                for id in ["v1b", "v2a", "v1c", "v1d"] {
                    let _ = ctx.verified_by_id(c, id)?;
                }
                assert_eq!(ctx.fixed_runs, 5);
                // v1a was evicted (cap 4); revisiting re-verifies it.
                let (_, again) = ctx.verified_by_id(c, "v1a")?;
                assert_eq!(ctx.fixed_runs, 6);
                assert!(!std::rc::Rc::ptr_eq(&first, &again));
                assert_eq!(first.version, again.version);
                Ok::<_, Error>(())
            })
            .unwrap();
    }

    /// Round-2 R2/R6: measured counts for the complete audit read of one
    /// committed Append item inside a single transaction — the base and
    /// result versions share the verified checkpoint segment, so each
    /// immutable segment is decoded and verified once while retained.
    #[test]
    fn append_read_shares_verified_segments_measured() {
        let f = fixture();
        f.repo
            .read(|c| {
                let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                let _ = crate::sqlite_market::compact_results::read_update_audit_ctx(
                    c, &mut ctx, "upd-1",
                )?;
                // base v1a + result v2a verified once each.
                assert_eq!(ctx.fixed_runs, 2);
                assert_eq!(ctx.facts_misses, 2);
                // The shared checkpoint segment (v1a) and the delta
                // segment (v2a) were each fully read once; every later
                // use — flatten, apply_delta, the persisted-ops
                // comparison, the repeated audit read below — is a hit.
                let (reads, hits, _, _) = ctx.segments().stats();
                let distinct: i64 = c
                    .query_row(
                        "SELECT count(*) FROM (\
                            SELECT segment_key FROM version_checkpoint_clips WHERE version_key IN \
                                (SELECT version_key FROM data_versions WHERE version_id='v1a') \
                            UNION \
                            SELECT segment_key FROM version_delta_clips WHERE version_key IN \
                                (SELECT version_key FROM data_versions WHERE version_id='v2a'))",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(storage)?;
                assert_eq!(reads as i64, distinct);
                assert!(hits >= distinct as usize);
                // A second identical read inside the transaction is
                // fully served from verified state: zero new fixed runs
                // or segment reads.
                let _ = crate::sqlite_market::compact_results::read_update_audit_ctx(
                    c, &mut ctx, "upd-1",
                )?;
                assert_eq!(ctx.fixed_runs, 2);
                assert_eq!(ctx.segments().stats().0, reads);
                Ok::<_, Error>(())
            })
            .unwrap();
    }

    /// Round-2 R2 / XP-016 case 1: measured lower-layer execution counts
    /// for one complete public `get_task_progress` read (fresh page).
    /// The instrumentation counts actual `read_segment`/`fixed`/
    /// `result_facts` executions — not `Rc::ptr_eq` extrapolation.
    /// XP-016 consolidates the whole RPC into ONE read transaction with
    /// ONE `VerifyCtx`: v1a and v2a are verified once and their
    /// certificates carry every later metadata touch (was 5/5/6 across
    /// three transactions).
    #[test]
    fn measured_counts_for_complete_public_read() {
        use crate::sqlite_market::compact_versions::read_counts;
        let f = fixture();
        read_counts::reset();
        let page = optimized_progress(&f, None, 100, &f.access).unwrap();
        let (segment_reads, fixed_runs, facts_reads) = read_counts::snapshot();
        // One transaction: task_facts (v1a+v2a verified once each),
        // verify_versions and verify_range_results served by the same
        // ctx — no re-verification across phases.
        assert_eq!(
            (fixed_runs, facts_reads, segment_reads),
            (2, 2, 2),
            "measured counts: fixed={fixed_runs} facts={facts_reads} segments={segment_reads}"
        );
        // Same full wire result as before consolidation.
        assert_eq!(page.task.task_id, f.task);
        assert_eq!(page.total, 3);
        assert_eq!(page.finished, 1);
        assert_eq!(page.succeeded, 1);
        assert_eq!(page.failed, 0);
        assert_eq!(page.publication_versions.len(), 1);
        assert_eq!(page.range_results.len(), 1);
        assert_eq!(page.unfinished.len(), 2);
        assert!(page.page_complete);
    }

    /// XP-016 (GOAL-008): one storage read transaction + one `VerifyCtx`
    /// per progress RPC; verified facts/certificates are
    /// snapshot-scoped, never cross-request trust.
    mod xp016 {
        use super::*;
        use crate::data_supply::observe::{AvailabilityFacts, ProgressRead, TaskFacts};
        use crate::data_supply::wire::{RangeExecutionResult, VersionRef, YearlyExecutionResult};
        use std::cell::{Cell, RefCell};

        /// Case-2 fixture: three published bases plus three committed
        /// append results — six distinct version keys, beyond the
        /// whole-history LRU bound of 4.
        fn fixture_three() -> Fixture {
            let dir = tempdir();
            let (path, repo) = repo(&dir);
            let bars = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
            publish_head(&repo, "req-a", "pub-a", "sh.600006", "v1a", bars.clone());
            publish_head(&repo, "req-b", "pub-b", "sh.600007", "v1b", bars.clone());
            publish_head(&repo, "req-c", "pub-c", "sh.600010", "v1c", bars);
            let scope_a = task_control::legacy::scope_id("sh.600006", "day");
            let scope_b = task_control::legacy::scope_id("sh.600007", "day");
            let scope_c = task_control::legacy::scope_id("sh.600010", "day");
            let item = |item_id: &str, symbol: &str, scope: &str, base: &str| ItemSpec {
                item_id: item_id.into(),
                target: json!({"symbol": symbol, "period": "day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: crate::market_update::tasks::frozen(
                    base,
                    "ab".repeat(32).as_str(),
                    &crate::market_update::DateRange {
                        start: date(0),
                        end: date(3),
                    },
                ),
                scope_keys: BTreeSet::from([scope.to_string()]),
            };
            let work = FrozenWork {
                items: vec![
                    item("item-1", "sh.600006", &scope_a, "v1a"),
                    item("item-2", "sh.600007", &scope_b, "v1b"),
                    item("item-3", "sh.600010", &scope_c, "v1c"),
                ],
                scopes: BTreeSet::from([scope_a.clone(), scope_b.clone(), scope_c.clone()]),
                shared_evidence: None,
            };
            let command = Command {
                key: CommandKey {
                    operation: "update_data".into(),
                    dataset: "market.daily".into(),
                    contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
                },
                intent: json!({}),
            };
            let task = match repo
                .accept("req-upd", &command, &work, RUN, None)
                .unwrap()
                .record
                .outcome
            {
                task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
                _ => panic!("expected task outcome"),
            };
            let commit = |item_id: &str,
                          publication_id: &str,
                          symbol: &str,
                          scope: &str,
                          base: &str,
                          result: &str| {
                UpdateBatchItem {
                    task_id: task.clone(),
                    item_id: item_id.into(),
                    publication_id: publication_id.into(),
                    run_id: RUN.into(),
                    symbol: symbol.into(),
                    occupancy_key: scope.to_string(),
                    prepared: UpdateBatchPrepared::Append {
                        base_version_id: base.into(),
                        result_version_id: result.into(),
                        upserts: vec![bar(3, 130)],
                        metadata: None,
                        continuity: json!({
                            "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}
                        }),
                    },
                }
            };
            repo.commit_update_batch(
                &task,
                &[
                    commit("item-1", "upd-1", "sh.600006", &scope_a, "v1a", "v2a"),
                    commit("item-2", "upd-2", "sh.600007", &scope_b, "v1b", "v2b"),
                    commit("item-3", "upd-3", "sh.600010", &scope_c, "v1c", "v2c"),
                ],
            )
            .unwrap()
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
            Fixture {
                _dir: dir,
                path,
                repo,
                task,
                access: Access::local(),
            }
        }

        /// Case-2 fixture at the stated batch (XP-016B B1): one hundred
        /// published bases plus one hundred committed append results —
        /// 200 distinct version keys. Same real accept/commit shape as
        /// `fixture_three`, scaled to 100 distinct entities.
        fn fixture_hundred() -> Fixture {
            let dir = tempdir();
            let (path, repo) = repo(&dir);
            let bars = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
            let mut items = Vec::new();
            let mut scopes = BTreeSet::new();
            let mut commits = Vec::new();
            for i in 0..100u32 {
                let symbol = format!("sh.{}", 800000 + i);
                let base = format!("v1h{i}");
                let result = format!("v2h{i}");
                publish_head(
                    &repo,
                    &format!("req-h{i}"),
                    &format!("pub-h{i}"),
                    &symbol,
                    &base,
                    bars.clone(),
                );
                let scope = task_control::legacy::scope_id(&symbol, "day");
                items.push(ItemSpec {
                    item_id: format!("item-{i}"),
                    target: json!({"symbol": symbol, "period": "day"}),
                    progress_unit: "security_period".into(),
                    frozen_inputs: crate::market_update::tasks::frozen(
                        &base,
                        "ab".repeat(32).as_str(),
                        &crate::market_update::DateRange {
                            start: date(0),
                            end: date(3),
                        },
                    ),
                    scope_keys: BTreeSet::from([scope.clone()]),
                });
                scopes.insert(scope.clone());
                commits.push((i, symbol, scope, base, result));
            }
            let work = FrozenWork {
                items,
                scopes,
                shared_evidence: None,
            };
            let command = Command {
                key: CommandKey {
                    operation: "update_data".into(),
                    dataset: "market.daily".into(),
                    contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
                },
                intent: json!({}),
            };
            let task = match repo
                .accept("req-upd", &command, &work, RUN, None)
                .unwrap()
                .record
                .outcome
            {
                task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
                _ => panic!("expected task outcome"),
            };
            let batch: Vec<UpdateBatchItem> = commits
                .into_iter()
                .map(|(i, symbol, scope, base, result)| UpdateBatchItem {
                    task_id: task.clone(),
                    item_id: format!("item-{i}"),
                    publication_id: format!("upd-h{i}"),
                    run_id: RUN.into(),
                    symbol,
                    occupancy_key: scope,
                    prepared: UpdateBatchPrepared::Append {
                        base_version_id: base,
                        result_version_id: result,
                        upserts: vec![bar(3, 130)],
                        metadata: None,
                        continuity: json!({
                            "continuity": {"mode": "append", "day_member_sha256": "cd".repeat(32)}
                        }),
                    },
                })
                .collect();
            repo.commit_update_batch(&task, &batch)
                .unwrap()
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            Fixture {
                _dir: dir,
                path,
                repo,
                task,
                access: Access::local(),
            }
        }

        /// Same update task shape as `fixture()` — three items, two
        /// published bases — but nothing committed: the interrupt
        /// receipt fixture drives the task to NeedsVerification while
        /// all items are still unfinished.
        fn fixture_uncommitted() -> Fixture {
            let dir = tempdir();
            let (path, repo) = repo(&dir);
            let bars = vec![bar(0, 100), bar(1, 110), bar(2, 120)];
            publish_head(&repo, "req-a", "pub-a", "sh.600006", "v1a", bars.clone());
            publish_head(&repo, "req-b", "pub-b", "sh.600007", "v1b", bars);
            let scope_a = task_control::legacy::scope_id("sh.600006", "day");
            let scope_b = task_control::legacy::scope_id("sh.600007", "day");
            let item = |item_id: &str, symbol: &str, scope: &str, base: &str| ItemSpec {
                item_id: item_id.into(),
                target: json!({"symbol": symbol, "period": "day"}),
                progress_unit: "security_period".into(),
                frozen_inputs: crate::market_update::tasks::frozen(
                    base,
                    "ab".repeat(32).as_str(),
                    &crate::market_update::DateRange {
                        start: date(0),
                        end: date(3),
                    },
                ),
                scope_keys: BTreeSet::from([scope.to_string()]),
            };
            let work = FrozenWork {
                items: vec![
                    item("item-1", "sh.600006", &scope_a, "v1a"),
                    item("item-2", "sh.600007", &scope_b, "v1b"),
                    item("item-3", "sh.600006", &scope_a, "v1a"),
                ],
                scopes: BTreeSet::from([scope_a.clone(), scope_b]),
                shared_evidence: None,
            };
            let command = Command {
                key: CommandKey {
                    operation: "update_data".into(),
                    dataset: "market.daily".into(),
                    contract_version: crate::market_update::tasks::CONTRACT_VERSION.into(),
                },
                intent: json!({}),
            };
            let task = match repo
                .accept("req-upd", &command, &work, RUN, None)
                .unwrap()
                .record
                .outcome
            {
                task_control::RequestOutcome::Task(snapshot) => snapshot.task.task_id.clone(),
                _ => panic!("expected task outcome"),
            };
            claim(&repo, &task);
            Fixture {
                _dir: dir,
                path,
                repo,
                task,
                access: Access::local(),
            }
        }

        /// Case 2: six distinct version keys exceed the whole-history
        /// LRU bound (4). Every phase of the single progress RPC reuses
        /// certificates for metadata touches — a version whose retained
        /// history was LRU-evicted does NOT re-run `fixed` — while the
        /// very next RPC opens a fresh scope and verifies all six again.
        #[test]
        fn certificates_cover_metadata_after_lru_eviction_measured() {
            use crate::sqlite_market::compact_versions::read_counts;
            let f = fixture_three();
            read_counts::reset();
            let page = optimized_progress(&f, None, 100, &f.access).unwrap();
            let (segments, fixed, facts) = read_counts::snapshot();
            // v1a/v1b/v1c bases + v2a/v2b/v2c delta results: each
            // verified exactly once; certificate hits serve all later
            // metadata touches despite verified-LRU eviction.
            assert_eq!(
                (fixed, facts, segments),
                (6, 6, 6),
                "fixed={fixed} facts={facts} segments={segments}"
            );
            // Same classification/metadata as the uncached reference.
            assert_eq!(page.task.task_id, f.task);
            assert_eq!(page.total, 3);
            assert_eq!(page.finished, 3);
            assert_eq!(page.succeeded, 3);
            assert_eq!(page.failed, 0);
            assert_eq!(page.publication_versions.len(), 3);
            assert_eq!(page.range_results.len(), 3);
            assert!(page.page_complete);
            // No cross-request reuse: the next RPC is a fresh scope and
            // verifies all six versions again.
            read_counts::reset();
            optimized_progress(&f, None, 100, &f.access).unwrap();
            let (segments, fixed, facts) = read_counts::snapshot();
            assert_eq!((fixed, facts, segments), (6, 6, 6));
        }

        /// Case 2 at the stated batch (XP-016B B1): 100 distinct
        /// published bases plus 100 distinct committed append results —
        /// 200 version keys. A single fresh progress RPC verifies each
        /// exactly once (fixed=200, facts=200); the WHOLE serialized
        /// page matches the separate-phase `ForwardRepo` reference with
        /// only `snapshot_id`/`snapshot_time` normalized; a second RPC
        /// opens a fresh scope and verifies all 200 again. Segment
        /// reads are recorded, not asserted — the 128-entry cache may
        /// evict and re-read. No timing extrapolation.
        #[test]
        fn hundred_distinct_versions_whole_wire_matches_reference() {
            use crate::sqlite_market::compact_versions::read_counts;
            let f = fixture_hundred();
            read_counts::reset();
            let page = optimized_progress(&f, None, 100, &f.access).unwrap();
            let (segments, fixed, facts) = read_counts::snapshot();
            assert_eq!(
                (fixed, facts),
                (200, 200),
                "fixed={fixed} facts={facts} segments={segments} (recorded only)"
            );
            assert_eq!(page.task.task_id, f.task);
            assert_eq!(page.total, 100);
            assert_eq!(page.finished, 100);
            assert_eq!(page.succeeded, 100);
            assert_eq!(page.failed, 0);
            assert_eq!(page.publication_versions.len(), 100);
            assert_eq!(page.range_results.len(), 100);
            assert!(page.page_complete);
            assert!(page.next_cursor.is_none());
            // No cross-request reuse: the next RPC is a fresh scope and
            // verifies all 200 versions again.
            read_counts::reset();
            let page2 = optimized_progress(&f, None, 100, &f.access).unwrap();
            let (segments2, fixed2, facts2) = read_counts::snapshot();
            assert_eq!(
                (fixed2, facts2),
                (200, 200),
                "second RPC fixed={fixed2} facts={facts2} segments={segments2} (recorded only)"
            );
            assert!(page2.page_complete);
            assert!(page2.next_cursor.is_none());
            // Whole-wire equivalence with the pre-consolidation
            // reference: `ForwardRepo` does not override
            // `with_progress_read`, so the default adapter runs every
            // verifier in its own separate read phase.
            let reference = Observer::new(
                ForwardRepo {
                    inner: supply(&f.repo),
                    calls: RefCell::new(vec![]),
                },
                RUN.into(),
            )
            .progress(
                ProgressQuery {
                    task_id: f.task.clone(),
                    page_size: 100,
                    cursor: None,
                },
                &f.access,
            )
            .unwrap();
            assert!(reference.page_complete);
            assert!(reference.next_cursor.is_none());
            let normalized = |page: &crate::data_supply::wire::ProgressPage| {
                let mut v = serde_json::to_value(page).unwrap();
                v["snapshot_id"] = json!("<normalized>");
                v["snapshot_time"] = json!("<normalized>");
                v
            };
            let reference_json = normalized(&reference);
            assert_eq!(normalized(&page), reference_json);
            assert_eq!(normalized(&page2), reference_json);
        }

        /// Case 3: >512 published keys — certificate FIFO eviction makes
        /// a revisit fully re-verify; a bounded facts hit never grants a
        /// certificate on its own.
        #[test]
        fn certificate_fifo_eviction_reverifies_and_facts_alone_not_certified() {
            let f = fixture();
            let mut ids = Vec::new();
            for i in 0..520u32 {
                let symbol = format!("sh.9{i:05}");
                let version_id = format!("vx{i:04}");
                publish_head(
                    &f.repo,
                    &format!("req-x{i}"),
                    &format!("pub-x{i}"),
                    &symbol,
                    &version_id,
                    vec![bar(0, 100)],
                );
                ids.push(version_id);
            }
            f.repo
                .read(|c| {
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    // Facts memo alone never grants a certificate.
                    let key0 = version_key(c, &ids[0])?;
                    let _ = ctx.facts(c, key0)?;
                    let _ = ctx.verified_facts(c, key0)?;
                    assert_eq!(ctx.fixed_runs, 1);
                    // Verify all 520: certificates cap at 512 FIFO.
                    for id in &ids[1..] {
                        let _ = ctx.verified_facts_by_id(c, id)?;
                    }
                    assert_eq!(ctx.fixed_runs, 520);
                    // The first certificate was evicted — the revisit
                    // runs full verification again.
                    let _ = ctx.verified_facts(c, key0)?;
                    assert_eq!(ctx.fixed_runs, 521);
                    // A still-retained certificate stays a metadata hit.
                    let _ = ctx.verified_facts_by_id(c, &ids[519])?;
                    assert_eq!(ctx.fixed_runs, 521);
                    Ok::<_, Error>(())
                })
                .unwrap();
        }

        /// Case 3 (fail-closed halves): a corrupt first touch fails and
        /// leaves no certificate — a second touch on the SAME ctx runs
        /// real verification again and fails again (lower fixed-entry
        /// counter == 2); a whole-bar read after whole-history LRU
        /// eviction reconstructs bars identical to the uncached read
        /// even though a certificate remains.
        #[test]
        fn failed_first_touch_no_certificate_and_whole_bars_reconstruct() {
            let f = fixture();
            publish_head(
                &f.repo,
                "req-bad",
                "pub-bad",
                "sh.600020",
                "vbad",
                vec![bar(0, 100)],
            );
            for (req, sym, id) in [
                ("req-1", "sh.600021", "w1"),
                ("req-2", "sh.600022", "w2"),
                ("req-3", "sh.600023", "w3"),
                ("req-4", "sh.600024", "w4"),
            ] {
                publish_head(&f.repo, req, &format!("p-{id}"), sym, id, vec![bar(0, 100)]);
            }
            let clip_segment = "SELECT segment_key FROM version_checkpoint_clips \
                 WHERE version_key=(SELECT version_key FROM data_versions WHERE version_id='vbad')";
            tamper(
                &f.path,
                &format!(
                    "UPDATE segment_bars SET close_cents=close_cents+1 WHERE segment_key IN ({clip_segment})"
                ),
            );
            f.repo
                .read(|c| {
                    use crate::sqlite_market::compact_versions::read_counts;
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    // Same ctx, twice: a failed first touch leaves no
                    // certificate, so the retry must run real
                    // verification again — and fail again.
                    read_counts::reset();
                    assert!(ctx.verified_facts_by_id(c, "vbad").is_err());
                    assert!(ctx.verified_facts_by_id(c, "vbad").is_err());
                    // `ctx.fixed_runs` counts successful runs only; the
                    // lower fixed-entry counter proves both touches
                    // actually executed verification.
                    let (_, fixed, _) = read_counts::snapshot();
                    assert_eq!(fixed, 2);
                    assert_eq!(ctx.fixed_runs, 0);
                    Ok::<_, Error>(())
                })
                .unwrap();
            // Repair — the next touch runs real verification again; a
            // phantom certificate would have skipped it.
            tamper(
                &f.path,
                &format!(
                    "UPDATE segment_bars SET close_cents=close_cents-1 WHERE segment_key IN ({clip_segment})"
                ),
            );
            f.repo
                .read(|c| {
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    assert!(ctx.verified_facts_by_id(c, "vbad").is_ok());
                    assert_eq!(ctx.fixed_runs, 1);
                    Ok::<_, Error>(())
                })
                .unwrap();
            // Whole-bar read after whole-history LRU eviction: w1's
            // certificate survives (cap 512) while its VerifiedVersion
            // was evicted (cap 4) — the bars are reconstructed and equal
            // the uncached read.
            f.repo
                .read(|c| {
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    let _ = ctx.verified_by_id(c, "w1")?;
                    for id in ["w2", "w3", "w4", "vbad"] {
                        let _ = ctx.verified_by_id(c, id)?;
                    }
                    assert_eq!(ctx.fixed_runs, 5);
                    let (_, rebuilt) = ctx.verified_by_id(c, "w1")?;
                    assert_eq!(ctx.fixed_runs, 6);
                    let uncached = crate::sqlite_market::compact_versions::fixed(c, "w1")?;
                    assert_eq!(rebuilt.version, uncached.version);
                    Ok::<_, Error>(())
                })
                .unwrap();
        }

        /// Case 4: a `VerifyCtx` binds to one `Connection` — reuse from
        /// a second connection fails closed on facts hits, certificate
        /// hits and fresh lookups alike. (Oversized/unretained segment
        /// behavior is covered by the compact_v19 segment-ctx test.)
        #[test]
        fn verify_ctx_bound_to_one_connection() {
            let f = fixture();
            f.repo
                .read(|c| {
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    let key = version_key(c, "v2a")?;
                    let _ = ctx.facts(c, key)?;
                    let _ = ctx.verified_facts(c, key)?;
                    let other = rusqlite::Connection::open(&f.path).unwrap();
                    assert!(matches!(ctx.facts(&other, key), Err(Error::Integrity(_))));
                    assert!(matches!(
                        ctx.verified_facts(&other, key),
                        Err(Error::Integrity(_))
                    ));
                    assert!(matches!(
                        ctx.verified(&other, key),
                        Err(Error::Integrity(_))
                    ));
                    Ok::<_, Error>(())
                })
                .unwrap();
        }

        /// Case 5: recording `ProgressRead` — fresh progress calls
        /// `task` then every verifier; a cursor page calls every
        /// verifier again inside a NEW `with_progress_read` scope; any
        /// injected verifier error fails the RPC (no successful page).
        struct RecordingRead<'a> {
            inner: &'a dyn ProgressRead,
            calls: &'a RefCell<Vec<String>>,
            fail: &'a RefCell<Option<&'static str>>,
            scope: usize,
        }
        impl RecordingRead<'_> {
            fn hit<T>(&self, name: &'static str, r: Result<T, Error>) -> Result<T, Error> {
                self.calls
                    .borrow_mut()
                    .push(format!("s{}.{}", self.scope, name));
                if self.fail.borrow().as_deref() == Some(name) {
                    return Err(Error::Invalid(format!("injected {name} failure")));
                }
                r
            }
        }
        impl ProgressRead for RecordingRead<'_> {
            fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
                self.hit("task", self.inner.task(id, access))
            }
            fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
                self.hit("verify_versions", self.inner.verify_versions(refs))
            }
            fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
                self.hit(
                    "verify_range_results",
                    self.inner.verify_range_results(results),
                )
            }
            fn verify_yearly_results(
                &self,
                results: &[YearlyExecutionResult],
            ) -> Result<(), Error> {
                self.hit(
                    "verify_yearly_results",
                    self.inner.verify_yearly_results(results),
                )
            }
            fn verify_publication_check(
                &self,
                receipt: &Option<crate::publication_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.hit(
                    "verify_publication_check",
                    self.inner.verify_publication_check(receipt),
                )
            }
            fn verify_stopped_check(
                &self,
                receipt: &Option<crate::task_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.hit(
                    "verify_stopped_check",
                    self.inner.verify_stopped_check(receipt),
                )
            }
        }
        struct RecordingRepo {
            inner: SqliteSupply,
            calls: RefCell<Vec<String>>,
            scopes: Cell<usize>,
            fail: RefCell<Option<&'static str>>,
        }
        impl ObservationRepository for RecordingRepo {
            fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error> {
                self.inner.tasks(access)
            }
            fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
                self.inner.task(id, access)
            }
            fn availability(
                &self,
                plan: &crate::data_supply::query::QueryPlan,
                access: &Access,
            ) -> Result<Vec<AvailabilityFacts>, Error> {
                self.inner.availability(plan, access)
            }
            fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
                self.inner.verify_versions(refs)
            }
            fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
                self.inner.verify_range_results(results)
            }
            fn verify_yearly_results(
                &self,
                results: &[YearlyExecutionResult],
            ) -> Result<(), Error> {
                self.inner.verify_yearly_results(results)
            }
            fn verify_publication_check(
                &self,
                receipt: &Option<crate::publication_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.inner.verify_publication_check(receipt)
            }
            fn verify_stopped_check(
                &self,
                receipt: &Option<crate::task_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.inner.verify_stopped_check(receipt)
            }
            fn with_progress_read<T>(
                &self,
                f: impl FnOnce(&dyn ProgressRead) -> Result<T, Error>,
            ) -> Result<T, Error> {
                self.scopes.set(self.scopes.get() + 1);
                let scope = self.scopes.get();
                self.inner.with_progress_read(|inner| {
                    f(&RecordingRead {
                        inner,
                        calls: &self.calls,
                        fail: &self.fail,
                        scope,
                    })
                })
            }
        }

        const SIX: [&str; 6] = [
            "task",
            "verify_versions",
            "verify_range_results",
            "verify_yearly_results",
            "verify_stopped_check",
            "verify_publication_check",
        ];

        /// Case 5 fresh + cursor call-sequence recording.
        #[test]
        fn progress_calls_all_six_methods_per_scope() {
            let f = fixture();
            let repo = RecordingRepo {
                inner: supply(&f.repo),
                calls: RefCell::new(vec![]),
                scopes: Cell::new(0),
                fail: RefCell::new(None),
            };
            let observer = Observer::new(repo, RUN.into());
            let query = |cursor: Option<String>| ProgressQuery {
                task_id: f.task.clone(),
                page_size: 1,
                cursor,
            };
            let page1 = observer.progress(query(None), &f.access).unwrap();
            let expected_fresh: Vec<String> = SIX.iter().map(|m| format!("s1.{m}")).collect();
            assert_eq!(*observer.repository.calls.borrow(), expected_fresh);
            let page2 = observer
                .progress(query(page1.next_cursor.clone()), &f.access)
                .unwrap();
            assert!(page2.next_cursor.is_some() || page2.page_complete);
            let expected_cursor: Vec<String> = SIX[1..].iter().map(|m| format!("s2.{m}")).collect();
            let calls = observer.repository.calls.borrow().clone();
            assert_eq!(calls[6..], expected_cursor[..]);
            assert_eq!(observer.repository.scopes.get(), 2);
        }

        /// Case 5 error injection: every injected verifier failure —
        /// including on cursor pages — fails the RPC outright.
        #[test]
        fn injected_verifier_error_fails_rpc_no_successful_page() {
            for name in SIX {
                let f = fixture();
                let repo = RecordingRepo {
                    inner: supply(&f.repo),
                    calls: RefCell::new(vec![]),
                    scopes: Cell::new(0),
                    fail: RefCell::new(Some(name)),
                };
                let observer = Observer::new(repo, RUN.into());
                let query = |cursor: Option<String>| ProgressQuery {
                    task_id: f.task.clone(),
                    page_size: 1,
                    cursor,
                };
                assert!(
                    observer.progress(query(None), &f.access).is_err(),
                    "injected {name} must fail fresh progress"
                );
                // Cursor page with an injected verifier error fails too —
                // no successful response is ever returned on error.
                if name != "task" {
                    let f2 = fixture();
                    let repo2 = RecordingRepo {
                        inner: supply(&f2.repo),
                        calls: RefCell::new(vec![]),
                        scopes: Cell::new(0),
                        fail: RefCell::new(Some(name)),
                    };
                    let observer2 = Observer::new(repo2, RUN.into());
                    let page1 = {
                        *observer2.repository.fail.borrow_mut() = None;
                        observer2
                            .progress(
                                ProgressQuery {
                                    task_id: f2.task.clone(),
                                    page_size: 1,
                                    cursor: None,
                                },
                                &f2.access,
                            )
                            .unwrap()
                    };
                    *observer2.repository.fail.borrow_mut() = Some(name);
                    assert!(
                        observer2
                            .progress(
                                ProgressQuery {
                                    task_id: f2.task.clone(),
                                    page_size: 1,
                                    cursor: page1.next_cursor.clone(),
                                },
                                &f2.access,
                            )
                            .is_err(),
                        "injected {name} must fail cursor progress"
                    );
                }
            }
        }

        /// Case 9 compat: the default `with_progress_read` forwarding
        /// adapter calls all six repository methods — non-SQLite
        /// repositories keep working without an override.
        struct ForwardRepo {
            inner: SqliteSupply,
            calls: RefCell<Vec<String>>,
        }
        impl ObservationRepository for ForwardRepo {
            fn tasks(&self, access: &Access) -> Result<Vec<TaskFacts>, Error> {
                self.inner.tasks(access)
            }
            fn task(&self, id: &str, access: &Access) -> Result<TaskFacts, Error> {
                self.calls.borrow_mut().push("task".into());
                self.inner.task(id, access)
            }
            fn availability(
                &self,
                plan: &crate::data_supply::query::QueryPlan,
                access: &Access,
            ) -> Result<Vec<AvailabilityFacts>, Error> {
                self.inner.availability(plan, access)
            }
            fn verify_versions(&self, refs: &[VersionRef]) -> Result<(), Error> {
                self.calls.borrow_mut().push("verify_versions".into());
                self.inner.verify_versions(refs)
            }
            fn verify_range_results(&self, results: &[RangeExecutionResult]) -> Result<(), Error> {
                self.calls.borrow_mut().push("verify_range_results".into());
                self.inner.verify_range_results(results)
            }
            fn verify_yearly_results(
                &self,
                results: &[YearlyExecutionResult],
            ) -> Result<(), Error> {
                self.calls.borrow_mut().push("verify_yearly_results".into());
                self.inner.verify_yearly_results(results)
            }
            fn verify_publication_check(
                &self,
                receipt: &Option<crate::publication_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.calls
                    .borrow_mut()
                    .push("verify_publication_check".into());
                self.inner.verify_publication_check(receipt)
            }
            fn verify_stopped_check(
                &self,
                receipt: &Option<crate::task_verification::decision::Receipt>,
            ) -> Result<(), Error> {
                self.calls.borrow_mut().push("verify_stopped_check".into());
                self.inner.verify_stopped_check(receipt)
            }
        }

        #[test]
        fn default_with_progress_read_forwards_all_six() {
            let f = fixture();
            let repo = ForwardRepo {
                inner: supply(&f.repo),
                calls: RefCell::new(vec![]),
            };
            let observer = Observer::new(repo, RUN.into());
            observer
                .progress(
                    ProgressQuery {
                        task_id: f.task.clone(),
                        page_size: 100,
                        cursor: None,
                    },
                    &f.access,
                )
                .unwrap();
            assert_eq!(
                *observer.repository.calls.borrow(),
                SIX.iter().map(|m| m.to_string()).collect::<Vec<_>>()
            );
        }

        /// Case 7: corruption committed between cursor pages fails the
        /// next page's fresh read scope AND a brand-new fresh RPC —
        /// nothing verified on page one survives into later snapshots.
        #[test]
        fn committed_corruption_fails_cursor_page_and_fresh_rpc() {
            let f = fixture();
            let observer = Observer::new(supply(&f.repo), RUN.into());
            let query = |cursor: Option<String>| ProgressQuery {
                task_id: f.task.clone(),
                page_size: 1,
                cursor,
            };
            let page1 = observer.progress(query(None), &f.access).unwrap();
            let cursor = page1.next_cursor.clone().expect("second page");
            tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
            assert!(
                observer.progress(query(Some(cursor)), &f.access).is_err(),
                "cursor page must open a fresh scope and fail"
            );
            assert!(
                observer.progress(query(None), &f.access).is_err(),
                "fresh RPC after commit must fail too"
            );
        }

        /// Case 8 (explicit MVCC linearization): inside one read
        /// transaction every remaining check sees the same consistent
        /// snapshot — a commit from a second connection mid-scope is
        /// invisible — while the NEXT RPC (new snapshot) observes and
        /// fails on the committed change. This also documents that a
        /// commit during an active RPC need not be detected until the
        /// next snapshot.
        #[test]
        fn mid_scope_commit_invisible_but_next_rpc_sees_it() {
            let f = fixture();
            f.repo
                .read(|c| {
                    let mut ctx = crate::sqlite_market::compact_versions::VerifyCtx::default();
                    // Establish the snapshot with a real verified read.
                    let _ = ctx.verified_by_id(c, "v1a")?;
                    // Second connection commits corruption on v2a.
                    tamper(&f.path, "UPDATE segment_bars SET close_cents=close_cents+1");
                    // A never-before-verified version read inside THIS
                    // scope still sees the pre-commit snapshot.
                    let _ = ctx.verified_by_id(c, "v2a")?;
                    Ok::<_, Error>(())
                })
                .unwrap();
            assert_eq!(
                read_class(&optimized_progress(&f, None, 100, &f.access)),
                ReadClass::Integrity
            );
        }

        /// A `task.verification` check task bound to `target` through
        /// the real accept/claim/start path — the same snapshots the
        /// check pipelines produce.
        fn check_task(
            repo: &SqliteCompactMarket,
            target_id: &str,
            target_run: &str,
            request: &str,
            item_id: &str,
            key: &CommandKey,
            run: &str,
        ) -> task_control::TaskSnapshot {
            let scope = crate::task_verification::verification_scope(target_id);
            let work = FrozenWork {
                items: vec![ItemSpec {
                    item_id: item_id.into(),
                    target: json!({"task_id": target_id}),
                    progress_unit: "evidence_check".into(),
                    frozen_inputs: json!({
                        "target_task_id": target_id,
                        "accepted_run_id": target_run,
                    }),
                    scope_keys: BTreeSet::from([scope.clone()]),
                }],
                scopes: BTreeSet::from([scope]),
                shared_evidence: None,
            };
            let command = Command {
                key: key.clone(),
                intent: json!({"target_task_id": target_id}),
            };
            let task_id = match repo
                .accept(request, &command, &work, run, None)
                .unwrap()
                .record
                .outcome
            {
                task_control::RequestOutcome::Task(s) => s.task.task_id.clone(),
                _ => panic!("expected task outcome"),
            };
            let snap = repo.transition(&task_id, 0, run, Change::Claim).unwrap();
            repo.transition(
                &task_id,
                snap.task.state_revision,
                run,
                Change::StartItem(item_id.into()),
            )
            .unwrap()
        }

        /// Case 9 (publication receipt): a committed `publication_check`
        /// receipt is compared inside the shared read scope on fresh
        /// progress — healthy passes; a tampered stored audit fails
        /// closed with the same classification as before consolidation.
        #[test]
        fn publication_receipt_healthy_and_tampered_progress() {
            let f = fixture();
            let target = f
                .repo
                .read(|c| Ok(crate::sqlite_tasks::read_bound_task(c, &f.task)?))
                .unwrap()
                .unwrap();
            let check = check_task(
                &f.repo,
                &f.task,
                &target.task.accepted_run_id,
                "req-pcheck",
                "publication-results",
                &crate::publication_verification::admission::command_key(),
                RUN,
            );
            let check_id = check.task.task_id.clone();
            let facts = f
                .repo
                .read(|c| crate::sqlite_market::verification::read_facts(c, &f.task))
                .unwrap();
            let plan = crate::publication_verification::decision::prepare(
                check,
                facts,
                RUN,
                "fixture-checker",
                ts(),
            )
            .unwrap();
            let receipt =
                crate::publication_verification::decision::AuditStore::commit_check(&f.repo, &plan)
                    .unwrap();
            // Healthy: fresh progress compares the stored receipt inside
            // the same read scope and returns it on the wire.
            let observer = Observer::new(supply(&f.repo), RUN.into());
            let query = |cursor: Option<String>| ProgressQuery {
                task_id: check_id.clone(),
                page_size: 1,
                cursor,
            };
            let page = observer.progress(query(None), &f.access).unwrap();
            assert_eq!(page.publication_check, Some(receipt));
            // Tampered stored audit fails closed on the next fresh RPC:
            // the bound-task link validation inside `task_facts` hits the
            // corrupted audit first — NeedsVerification class, identical
            // to the pre-consolidation surface.
            tamper(
                &f.path,
                "UPDATE publication_checks SET audit_sha256='damaged'",
            );
            assert_eq!(
                read_class(&observer.progress(query(None), &f.access)),
                ReadClass::TaskError
            );
        }

        /// Case 9 (stopped receipt, fresh + cursor): the interrupted
        /// target task carries the committed `stopped_write_check`
        /// receipt — every progress page re-verifies it inside a fresh
        /// read scope; a tampered stored audit fails closed.
        #[test]
        fn stopped_receipt_healthy_and_tampered_fresh_and_cursor() {
            struct Stopped;
            impl crate::task_verification::decision::StopInspector for Stopped {
                fn inspect(
                    &self,
                    s: &task_control::TaskSnapshot,
                ) -> Result<crate::task_verification::decision::Observation, task_control::Error>
                {
                    Ok(crate::task_verification::decision::Observation::Stopped {
                        snapshot_sha256: crate::task_verification::decision::snapshot_digest(s)?,
                        evidence_ref: "fixture-writer-exit".into(),
                    })
                }
            }
            // Same task shape as `fixture()` but without committed
            // results: the interrupt path rewrites the whole target
            // snapshot, and a committed compact result's canonical
            // outcome_json is not byte-preserved by a whole-snapshot
            // rewrite — the receipt surface under test is exercised on
            // an uncommitted task instead.
            let f = fixture_uncommitted();
            // Drive item-1 into a running state, then require
            // verification — the honest NeedsVerification transition.
            let rev = f.repo.task(&f.task).unwrap().task.state_revision;
            let snap = f
                .repo
                .transition(&f.task, rev, RUN, Change::StartItem("item-1".into()))
                .unwrap();
            let snap = f
                .repo
                .transition(
                    &f.task,
                    snap.task.state_revision,
                    RUN,
                    Change::RequireVerification {
                        item_id: "item-1".into(),
                        reason: "fixture uncertain outcome".into(),
                    },
                )
                .unwrap();
            assert_eq!(snap.task.status, task_control::Status::NeedsVerification);
            let check = check_task(
                &f.repo,
                &f.task,
                &snap.task.accepted_run_id,
                "req-scheck",
                "stopped-writes",
                &crate::task_verification::command_key(),
                RUN,
            );
            let target = f
                .repo
                .read(|c| Ok(crate::sqlite_tasks::read_bound_task(c, &f.task)?))
                .unwrap()
                .unwrap();
            let plan = crate::task_verification::decision::prepare(
                check,
                target,
                RUN,
                "fixture-checker",
                ts(),
                &Stopped,
            )
            .unwrap();
            let receipt = crate::task_verification::decision::StopRepository::commit_stop_check(
                &f.repo.tasks(),
                &plan,
            )
            .unwrap();
            // The target is now Interrupted and carries the receipt;
            // two unfinished items make progress paginate.
            let observer = Observer::new(supply(&f.repo), RUN.into());
            let query = |cursor: Option<String>| ProgressQuery {
                task_id: f.task.clone(),
                page_size: 1,
                cursor,
            };
            let page1 = observer.progress(query(None), &f.access).unwrap();
            assert_eq!(page1.stopped_write_check, Some(receipt.clone()));
            let page2 = observer
                .progress(query(page1.next_cursor.clone()), &f.access)
                .unwrap();
            assert_eq!(page2.stopped_write_check, Some(receipt));
            // Deleted stored audit fails the NEXT cursor page (fresh
            // read scope) and a brand-new RPC identically — the
            // interrupted target's bound-task link validation reports
            // the missing audit as NeedsVerification on both paths.
            tamper(&f.path, "DELETE FROM stopped_write_checks");
            assert_eq!(
                read_class(&observer.progress(query(page2.next_cursor.clone()), &f.access)),
                ReadClass::TaskError
            );
            assert_eq!(
                read_class(&observer.progress(query(None), &f.access)),
                ReadClass::TaskError
            );
        }
    }
}
