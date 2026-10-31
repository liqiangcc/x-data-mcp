//! Operator CLI for the M3 source store (design §9.4.2 / §9.4.4).
//!
//! PR-1a selects a CLI entry instead of maint socket tools: the store is
//! written by an operator action against the deployment state directory (and
//! by the one-off archive import), the maint socket is a read/forensics entry
//! that never writes, and the deploy script already runs binaries directly.
//! Keeping the entry out of the MCP registry also keeps the three static
//! allowlists and the documented per-entry tool counts unchanged.
//!
//! ```text
//! source_store replay --store <dir> [--scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all]
//!                     [--db <market.sqlite>] [--verify-only]
//! source_store verify --store <dir> [--scope ...] [--db <market.sqlite>]
//! source_store import --store <dir> --index <archive-index.jsonl> --blobs <dir>
//!                     [--tiers A,B] [--kind industry|exchange] [--batch <id>] [--dry-run]
//! source_store exempt --db <market.sqlite> --generation <manifest sha> \
//!                     --missing <key> --reason <text> --recorded-by <who> \
//!                     [--owner-approve-evidence]
//! ```
//!
//! `--db` is what turns a replay into a rebuild: M3 PR-2's `exchange` scope
//! replaces `exchange_source_snapshots`, M3 PR-6's `industry` scope replaces
//! `industry_snapshots`/`industry_assignments`, M3 PR-8's `generation_facts`
//! scope replaces the retirement gate's fact index, M3 PR-10's `risk_warnings`
//! scope replaces `risk_warning_intervals` and M3 PR-12's `security_names`
//! scope replaces `security_name_history` (with its `derived_dataset_versions`
//! registration), each in one transaction; `--verify-only` compares the
//! recomputed summary with the stored rows instead of writing. XDM-138's
//! `suspensions` scope additionally reads the database's `security_master`,
//! trading calendar and bar days, assembles the dataset with the daily item's
//! own function and fails closed with `suspension_replay_not_ready` when those
//! inputs are absent; it is run explicitly after they exist (not by
//! `--scope all`). Without `--db` a
//! scope only computes and reports its summary, so a read never touches a
//! database or creates the store. `verify` always compares, even if
//! `--verify-only` is also given.
//!
//! Every command prints one JSON object on stdout. Exit 0 means `ok` or
//! `store_empty`; a corrupt store, an unknown parser version, a drift verdict or
//! a failed rebuild exits 1; bad usage exits 2.

use std::path::PathBuf;

use x_data_mcp::generation_gate::{RetireExemption, timestamp};
use x_data_mcp::source_store::{SourceStore, import, replay};

const USAGE: &str = "\
usage:
  source_store replay --store <dir> \
[--scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all] \
[--db <market.sqlite>] [--verify-only]
  source_store verify --store <dir> \
[--scope industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all] \
[--db <market.sqlite>]
  source_store import --store <dir> --index <archive-index.jsonl> --blobs <dir> \
[--tiers A,B] [--kind industry|exchange] [--batch <id>] [--dry-run]
  source_store exempt --db <market.sqlite> --generation <manifest sha> \
--missing <key> --reason <text> --recorded-by <who> [--owner-approve-evidence]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match run(&args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}\n\n{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

fn run(args: &[String]) -> Result<i32, String> {
    let Some(command) = args.first().map(String::as_str) else {
        return Err("missing command".to_string());
    };
    if command == "--help" || command == "-h" || command == "help" {
        println!("{USAGE}");
        return Ok(0);
    }
    if !matches!(command, "replay" | "verify" | "import" | "exempt") {
        return Err(format!("unknown command {command}"));
    }
    let mut store: Option<PathBuf> = None;
    let mut database: Option<PathBuf> = None;
    let mut scope = "all".to_string();
    let mut verify_only = false;
    let mut index: Option<PathBuf> = None;
    let mut blobs: Option<PathBuf> = None;
    let mut tiers = vec!["A".to_string(), "B".to_string()];
    let mut kind: Option<String> = None;
    let mut batch: Option<String> = None;
    let mut dry_run = false;
    let mut generation: Option<String> = None;
    let mut missing: Option<String> = None;
    let mut reason: Option<String> = None;
    let mut recorded_by: Option<String> = None;
    let mut owner_approve_evidence = false;
    let mut position = 1;
    while position < args.len() {
        let flag = args[position].as_str();
        match flag {
            "--store" => store = Some(PathBuf::from(value(args, position + 1)?)),
            "--db" => database = Some(PathBuf::from(value(args, position + 1)?)),
            "--scope" => scope = value(args, position + 1)?,
            "--index" => index = Some(PathBuf::from(value(args, position + 1)?)),
            "--blobs" => blobs = Some(PathBuf::from(value(args, position + 1)?)),
            "--tiers" => {
                tiers = value(args, position + 1)?
                    .split(',')
                    .map(str::trim)
                    .filter(|tier| !tier.is_empty())
                    .map(str::to_string)
                    .collect();
                if tiers.is_empty() {
                    return Err("--tiers needs at least one tier".to_string());
                }
            }
            "--kind" => kind = Some(value(args, position + 1)?),
            "--batch" => batch = Some(value(args, position + 1)?),
            "--verify-only" => verify_only = true,
            "--dry-run" => dry_run = true,
            "--generation" => generation = Some(value(args, position + 1)?),
            "--missing" => missing = Some(value(args, position + 1)?),
            "--reason" => reason = Some(value(args, position + 1)?),
            "--recorded-by" => recorded_by = Some(value(args, position + 1)?),
            "--owner-approve-evidence" => owner_approve_evidence = true,
            other => return Err(format!("unknown argument {other}")),
        }
        position += if flag.starts_with("--") && needs_value(flag) {
            2
        } else {
            1
        };
    }
    if command == "exempt" {
        return exempt(
            database.ok_or_else(|| "--db is required".to_string())?,
            generation.ok_or_else(|| "--generation is required".to_string())?,
            missing.ok_or_else(|| "--missing is required".to_string())?,
            reason.ok_or_else(|| "--reason is required".to_string())?,
            recorded_by.ok_or_else(|| "--recorded-by is required".to_string())?,
            owner_approve_evidence,
        );
    }
    let store_path = store.ok_or_else(|| "--store is required".to_string())?;
    let store = SourceStore::new(store_path);
    let selected_scope = match scope.as_str() {
        "all" => None,
        other => Some(replay::Scope::parse(other).ok_or_else(|| {
            format!(
                "unknown scope {other} \
                 (industry|exchange|generation_facts|risk_warnings|security_names|suspensions|all)"
            )
        })?),
    };
    let request = replay::ReplayRequest::new(selected_scope, verify_only, database.as_deref());
    match command {
        "replay" => print_report(replay::replay(&store, &request)),
        "verify" => print_report(replay::verify(&store, &request)),
        "import" => {
            let index_path = index.ok_or_else(|| "--index is required".to_string())?;
            let blobs_dir = blobs.ok_or_else(|| "--blobs is required".to_string())?;
            let options = import::ImportOptions {
                tiers,
                kind,
                dry_run,
                batch,
            };
            match import::import(&store, &index_path, &blobs_dir, &options) {
                Ok(report) => {
                    print_json(&report)?;
                    Ok(0)
                }
                Err(error) => {
                    print_failure(error.code(), &error.to_string())?;
                    Ok(1)
                }
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// Record one `retire_exemptions` row (M3 PR-8, `#446` §8.3 step 4).
///
/// An exemption is not the default path: the reason and the recorder must be
/// non-blank, and an exemption that covers *missing source-store evidence*
/// needs the explicit owner flag. The command writes the market database, so
/// it must run while `data_service` is stopped (the service holds the
/// single-writer lease).
fn exempt(
    database: PathBuf,
    generation: String,
    missing: String,
    reason: String,
    recorded_by: String,
    owner_approve_evidence: bool,
) -> Result<i32, String> {
    if reason.trim().is_empty() {
        return Err("--reason must not be blank".to_string());
    }
    if recorded_by.trim().is_empty() {
        return Err("--recorded-by must not be blank".to_string());
    }
    if missing.trim().is_empty() {
        return Err("--missing must not be blank".to_string());
    }
    if !missing.starts_with(x_data_mcp::generation_gate::EVIDENCE_KEY_PREFIX)
        && !missing.starts_with(x_data_mcp::generation_gate::FACT_KEY_PREFIX)
    {
        return Err(format!(
            "{missing} is not a gate key; expected source_store_evidence:<role>:<source_id> \
             or derived_fact:<role>:<fact_kind>"
        ));
    }
    if missing.starts_with(x_data_mcp::generation_gate::EVIDENCE_KEY_PREFIX)
        && !owner_approve_evidence
    {
        return Err(format!(
            "{missing} excuses missing source-store evidence; that needs the \
             owner's explicit approval, pass --owner-approve-evidence only after \
             it is recorded"
        ));
    }
    let tasks = x_data_mcp::sqlite_tasks::SqliteTasks::new(&database);
    let backend = x_data_mcp::sqlite_market::backend::MarketBackend::open(&tasks)
        .map_err(|error| format!("database_unavailable: {}: {error}", database.display()))?;
    let exemption = RetireExemption {
        generation_manifest_sha256: generation.clone(),
        missing_kind: missing.clone(),
        reason,
        recorded_by: recorded_by.clone(),
        recorded_at: timestamp(chrono::Utc::now()),
    };
    backend
        .record_retire_exemption(&exemption)
        .map_err(|error| error.to_string())?;
    print_json(&serde_json::json!({
        "status": "ok",
        "generation": generation,
        "missing_kind": missing,
        "recorded_by": recorded_by,
        "recorded_at": exemption.recorded_at,
    }))?;
    Ok(0)
}

fn needs_value(flag: &str) -> bool {
    matches!(
        flag,
        "--store"
            | "--db"
            | "--scope"
            | "--index"
            | "--blobs"
            | "--tiers"
            | "--kind"
            | "--batch"
            | "--generation"
            | "--missing"
            | "--reason"
            | "--recorded-by"
    )
}

fn value(args: &[String], position: usize) -> Result<String, String> {
    args.get(position)
        .cloned()
        .ok_or_else(|| format!("{} needs a value", args[position - 1]))
}

type ReplayResult = Result<replay::ReplayReport, x_data_mcp::source_store::StoreError>;

fn print_report(report: ReplayResult) -> Result<i32, String> {
    match report {
        Ok(report) => {
            let accepted = matches!(report.status, "ok" | "store_empty");
            print_json(&report)?;
            Ok(if accepted { 0 } else { 1 })
        }
        Err(error) => {
            print_failure(error.code(), &error.to_string())?;
            Ok(1)
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn print_failure(code: &str, detail: &str) -> Result<(), String> {
    print_json(&serde_json::json!({
        "status": "failed",
        "code": code,
        "detail": detail,
    }))
}
