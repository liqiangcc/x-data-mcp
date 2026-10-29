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
//! source_store replay --store <dir> [--scope industry|exchange|generation_facts|all]
//!                     [--db <market.sqlite>] [--verify-only]
//! source_store verify --store <dir> [--scope ...] [--db <market.sqlite>]
//! source_store import --store <dir> --index <archive-index.jsonl> --blobs <dir>
//!                     [--tiers A,B] [--kind industry|exchange] [--batch <id>] [--dry-run]
//! ```
//!
//! `--db` is what turns a replay into a rebuild: M3 PR-2's `exchange` scope
//! replaces `exchange_source_snapshots` from the store in one transaction and
//! `--verify-only` compares the recomputed summary with the stored rows instead
//! of writing. Without `--db` the scope only computes and reports its summary,
//! so a read never touches a database or creates the store.
//!
//! Every command prints one JSON object on stdout. Exit 0 means `ok` or
//! `store_empty`; a corrupt store, an unknown parser version, a drift verdict or
//! a failed rebuild exits 1; bad usage exits 2.

use std::path::PathBuf;

use x_data_mcp::source_store::{SourceStore, import, replay};

const USAGE: &str = "\
usage:
  source_store replay --store <dir> [--scope industry|exchange|generation_facts|all] \
[--db <market.sqlite>] [--verify-only]
  source_store verify --store <dir> [--scope industry|exchange|generation_facts|all] \
[--db <market.sqlite>]
  source_store import --store <dir> --index <archive-index.jsonl> --blobs <dir> \
[--tiers A,B] [--kind industry|exchange] [--batch <id>] [--dry-run]";

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
    if !matches!(command, "replay" | "verify" | "import") {
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
            other => return Err(format!("unknown argument {other}")),
        }
        position += if flag.starts_with("--") && needs_value(flag) {
            2
        } else {
            1
        };
    }
    let store_path = store.ok_or_else(|| "--store is required".to_string())?;
    let store = SourceStore::new(store_path);
    let selected_scope = match scope.as_str() {
        "all" => None,
        other => Some(replay::Scope::parse(other).ok_or_else(|| {
            format!("unknown scope {other} (industry|exchange|generation_facts|all)")
        })?),
    };
    match command {
        "replay" | "verify" => {
            let report = replay::replay_to(
                &store,
                selected_scope,
                verify_only || command == "verify",
                database.as_deref(),
            );
            print_report(report)
        }
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

fn needs_value(flag: &str) -> bool {
    matches!(
        flag,
        "--store" | "--db" | "--scope" | "--index" | "--blobs" | "--tiers" | "--kind" | "--batch"
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
