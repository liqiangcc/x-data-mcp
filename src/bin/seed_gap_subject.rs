//! Acceptance support only (Issue #187, frozen plan
//! `evidence/issue-187-backfill-acceptance/PLAN.md` §3): publish a
//! controlled local-gap subject through the governed task/publish path.
//! Never a production path: it refuses to run without an env-installed,
//! inspected official-file snapshot, and the produced version is labelled
//! an acceptance scratch subject in `coverage.reason`.
//!
//! usage: seed_gap_subject <database> --symbol <ex.code>
//!          [--drop-date <YYYY-MM-DD>] [--source official|fixture]
//!          [--request-id <id>] [--catalog-version <id>]
//!
//! `--drop-date` removes exactly that real source bar (it must exist in
//! the materialized member); omitting it publishes the full materialized
//! history as an acceptance subject (source-limitation/no-local-gap legs).
use chrono::Utc;
use serde_json::{Value, json};
use x_data_mcp::{
    catalog::{
        CatalogSnapshot, CompletenessEvidence, Exchange, IndustryInfo, SecurityRecord, SecurityType,
    },
    data_supply::{Access, mcp::DataApi},
    market_data::Period,
    market_storage::{MarketRepository, Publication, PublishIntent, Publisher, Scope},
    mcp::ToolHandler,
    official_market::{DailyCollectionRepository, TdxMarketSource},
    publish::{self, EffectiveVersion},
    sqlite_catalog::SqliteCatalog,
    sqlite_market::SqliteMarket,
    sqlite_tasks::SqliteTasks,
    task_control::Change,
};

fn usage() -> Box<dyn std::error::Error> {
    "usage: seed_gap_subject <database> --symbol <ex.code> [--drop-date <YYYY-MM-DD>] [--source official|fixture] [--request-id <id>] [--catalog-version <id>]"
        .into()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn opt(args: &[std::ffi::OsString], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.to_str().map(String::from))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return Err(usage());
    }
    let db = args[0].to_str().ok_or_else(usage)?.to_string();
    let symbol = opt(&args, "--symbol").ok_or_else(usage)?;
    let drop_date = opt(&args, "--drop-date")
        .map(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d"))
        .transpose()?;
    let fixture_source = match opt(&args, "--source").as_deref() {
        None | Some("official") => false,
        Some("fixture") => true,
        Some(_) => return Err("--source must be official or fixture".into()),
    };
    let request_id = opt(&args, "--request-id").unwrap_or_else(|| {
        format!(
            "seed-gap-{symbol}-{}",
            drop_date
                .map(|d| d.to_string())
                .unwrap_or_else(|| "full".into())
        )
    });
    let catalog_id =
        opt(&args, "--catalog-version").unwrap_or_else(|| "acceptance-187-catalog".into());
    let (exchange, code) = symbol.split_once('.').ok_or_else(|| usage())?;
    let exchange_enum = match exchange {
        "sh" => Exchange::Shanghai,
        "sz" => Exchange::Shenzhen,
        _ => return Err("symbol exchange must be sh or sz".into()),
    };

    // Refuse without a real inspected snapshot — no fixture bytes may ever
    // stand in for the official source.
    let source = TdxMarketSource::from_env()
        .ok_or("official-file source not installed: set X_DATA_MCP_OFFICIAL_MANIFEST and X_DATA_MCP_OFFICIAL_DIR to an inspected snapshot")?;

    let tasks = SqliteTasks::new(&db);
    let market = SqliteMarket::new(tasks.clone());
    let catalog = SqliteCatalog::new(tasks.clone());

    // Minimal fixture catalog so the default selector's catalog ∩
    // ever-started intersection sees the symbol; honestly labelled, never
    // presented as the official directory.
    let facts = catalog
        .selection_facts()
        .map_err(|e| -> Box<dyn std::error::Error> {
            format!("{e} (run `task_sqlite init <db>` first)").into()
        })?;
    if facts.catalog_head.is_none() {
        catalog.publish_fixture(
            &catalog_id,
            CatalogSnapshot {
                source: "fixture:acceptance-187".into(),
                acquired_at: Utc::now(),
                completeness: CompletenessEvidence::Complete { expected_count: 1 },
                records: vec![SecurityRecord {
                    symbol: code.into(),
                    name: code.into(),
                    exchange: exchange_enum,
                    security_type: SecurityType::AShare,
                    source_security_id: code.into(),
                    industry: IndustryInfo::Missing {
                        reason: "acceptance fixture".into(),
                    },
                    source: "fixture:acceptance-187".into(),
                    acquired_at: Utc::now(),
                }],
            },
            None,
        )?;
    }

    // Accept a real collection task through the same API the service uses —
    // no raw inserts; the task also provides the ever-started marker. The
    // lifecycle transitions below must run under the SAME run identity the
    // task was accepted with or the stale-run guard fails closed.
    let run_id = format!("seed-gap-subject-{}", Utc::now().timestamp_millis());
    let api = DataApi::new(tasks.clone(), run_id.clone(), Access::local())?;
    let accepted = api
        .call(
            "start_collection",
            json!({
                "request_id": request_id,
                "dataset": "market.daily",
                "contract_version": "v1",
                "selection": {"kind":"explicit","securities":[{"exchange":exchange,"code":code}]},
            }),
        )
        .map_err(|e| -> Box<dyn std::error::Error> { format!("{e:?}").into() })?;
    let task_id = accepted["task_id"]
        .as_str()
        .ok_or("start_collection acceptance carried no task_id")?
        .to_string();
    let task = market.task(&task_id)?;
    let item = task
        .items
        .first()
        .ok_or("accepted task carries no items")?
        .clone();
    let item_id = item.spec.item_id.clone();

    // Idempotent re-seed: the same request replays the same task; if the
    // item already published, report it rather than fabricating a second.
    if let Some(receipt) = market.collection_receipt(&task_id, &item_id)? {
        let version = market.fixed(&receipt.version_id)?;
        println!(
            "{}",
            json!({
                "reused": true,
                "task_id": task_id,
                "item_id": item_id,
                "version_id": receipt.version_id,
                "integrity_sha256": version.raw()["integrity_sha256"],
                "bars": version.version().records.len(),
            })
        );
        return Ok(());
    }

    // Materialize the real official QFQ history and remove exactly the
    // frozen target bar (which must exist in the member).
    let mut batch = source.materialize_daily(&symbol)?;
    if let Some(date) = drop_date {
        if !batch.bars.iter().any(|b| b.date == date) {
            return Err(format!("source member carries no bar for {date}").into());
        }
        batch.bars.retain(|b| b.date != date);
        // compat::check requires status Complete; the disclosure is carried
        // verbatim in the reason so the subject never reads as a complete
        // source history.
        batch.coverage.reason = format!(
            "ACCEPTANCE-LOCAL-GAP-SUBJECT: official {symbol} QFQ bars minus frozen target {date}; complete only for this declared scratch coverage, not complete source history"
        );
    } else {
        batch.coverage.reason = format!(
            "ACCEPTANCE-SUBJECT: official {symbol} QFQ bars as materialized; declared scratch coverage only, not a verified complete-history claim"
        );
    }
    if fixture_source {
        // --source fixture variant (plan §6-R5): real bars under a fixture
        // label so the verifier's source-identity check fails closed.
        batch.provenance.source = "fixture:acceptance-187".into();
    }

    let id = crate_hash(&task_id, &item_id);
    let candidate_id = format!("collection-version-{id}");
    let (records, validation) = publish::validate_and_deduplicate(&batch)?;
    let mut raw = serde_json::to_value(EffectiveVersion {
        version_id: candidate_id.clone(),
        symbol: batch.symbol.clone(),
        period: batch.period,
        adjustment: batch.adjustment,
        source: batch.provenance.source.clone(),
        records,
        published_at: Utc::now(),
        validation,
        unit_currency: batch.unit_currency.clone(),
        adjustment_evidence: batch.adjustment_evidence.clone(),
        yearly_derivation: None,
    })?;
    raw["batch"] = serde_json::to_value(&batch)?;
    raw["integrity_sha256"] =
        Value::String(x_data_mcp::market_storage::compat::legacy_digest(&raw)?);
    market.stage(&raw)?;

    // Governed lifecycle: claim, start, publish via Publisher with the
    // head the subject is built on — expected-head guard stays in force.
    let scope = Scope {
        symbol: symbol.clone(),
        period: Period::Day,
    };
    let expected_head = market.effective_head(&scope)?;
    let mut task = market.task(&task_id)?;
    if task.task.status == x_data_mcp::task_control::Status::Pending {
        task =
            market.transition_task(&task_id, task.task.state_revision, &run_id, Change::Claim)?;
    }
    task = market.transition_task(
        &task_id,
        task.task.state_revision,
        &run_id,
        Change::StartItem(item_id.clone()),
    )?;
    let plan = (Publisher {
        repository: market.clone(),
    })
    .prepare(Publication {
        publication_id: format!("collection-result-{id}"),
        version_id: candidate_id.clone(),
        task_id: task_id.clone(),
        item_id: item_id.clone(),
        run_id,
        expected_revision: task.task.state_revision,
        intent: PublishIntent::Daily { expected_head },
    })?;
    let receipt = market.commit(&plan)?;

    let version = market.fixed(&receipt.version_id)?;
    println!(
        "{}",
        json!({
            "reused": false,
            "task_id": task_id,
            "item_id": item_id,
            "version_id": receipt.version_id,
            "integrity_sha256": version.raw()["integrity_sha256"],
            "bars": version.version().records.len(),
            "drop_date": drop_date.map(|d| d.to_string()),
            "source_label": batch.provenance.source,
            "subject": "acceptance scratch subject — not a complete source history",
        })
    );
    Ok(())
}

/// Same candidate-id derivation `TdxDailyRunner` uses: sha256 of
/// `[task_id, item_id]` so the seeded version is bound to the real task.
fn crate_hash(task_id: &str, item_id: &str) -> String {
    x_data_mcp::hash::sha256(json!([task_id, item_id]).to_string().as_bytes())
}
