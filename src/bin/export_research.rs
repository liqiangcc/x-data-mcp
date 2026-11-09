//! `export_research`: the research snapshot CLI (issue-383 §7.1, M1 PR-8).
//!
//! Adapter only (separation point 5): argument parsing, one call into
//! `x_data_mcp::research`, one line of output. Every selection rule, the
//! four-layer binding and the file layout live in the library, so the same
//! snapshot can be produced by a test without going through this binary.
//!
//! `--list-snapshots` (D-83) is the only other mode: a read-only listing of the
//! export root, with no way to delete anything. It exits 2 when the listing
//! shows a damaged snapshot, an unexpected entry or a generation state it could
//! not read, so a script cannot take a partial answer for a clean one.
//!
//! Failure semantics (§7.1): a non-zero exit and one structured line
//! (`EXPORT_DRIFT: …`); nothing half-written is left behind because the
//! library writes into `<out>/.tmp-<pid>/` and only renames after the manifest
//! exists.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::NaiveDate;
use serde_json::json;
use x_data_mcp::research::export::{Compression, ExportOptions, dry_run, export};
use x_data_mcp::research::retention::{GenerationStates, list_snapshots};
use x_data_mcp::research::{
    AdjustmentSelection, DEFAULT_OUTPUT_ROOT, DatasetSelection, EntitySelection, ExportRequest,
};

const USAGE: &str = "usage: export_research --db <sqlite-path> [--out <dir>] \
[--dataset all|daily|index|calendar|factors|security] [--adjustment both|raw|qfq] \
[--start YYYY-MM-DD] [--end YYYY-MM-DD] \
[--entities sh.600000,sz.000001 | --catalog-version <id>] \
[--compression snappy|none] [--row-group-rows N] [--dry-run] [--quiet]\n\
       export_research --list-snapshots [--out <dir>] [--db <sqlite-path>] [--acquire-db <path>] [--quiet]\n\
\n\
X_DATA_MCP_SOURCE_STORE names the source store the ETF 前复权 check records \
live in. It is the same variable data_service reads. Unset or unreadable is not \
an error: no ETF enters daily_qfq and manifest.etf_forward.blocked names each of \
them with its reason.";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(reason) => {
            eprintln!("{reason}");
            ExitCode::FAILURE
        }
    }
}

fn invalid(message: impl std::fmt::Display) -> String {
    format!("EXPORT_INVALID: {message}")
}

fn date(flag: &str, text: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .map_err(|_| invalid(format!("{flag} {text} is not a YYYY-MM-DD date")))
}

fn run() -> Result<ExitCode, String> {
    let mut db: Option<PathBuf> = None;
    let mut out_root = PathBuf::from(DEFAULT_OUTPUT_ROOT);
    let mut dataset = DatasetSelection::All;
    let mut adjustment = AdjustmentSelection::Both;
    let mut start = None;
    let mut end = None;
    let mut entities = EntitySelection::CatalogHead;
    let mut entity_list_seen = false;
    let mut catalog_version_seen = false;
    let mut compression = Compression::Snappy;
    let mut row_group_rows = ExportOptions::default().row_group_rows;
    let mut dry_run_requested = false;
    let mut quiet = false;
    let mut list_requested = false;
    let mut acquire_db: Option<PathBuf> = None;
    let mut selection_flag: Option<String> = None;

    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|_| invalid("arguments must be UTF-8"))?;
        let mut value = |flag: &str| -> Result<String, String> {
            args.next()
                .ok_or_else(|| invalid(format!("{flag} needs a value")))?
                .into_string()
                .map_err(|_| invalid(format!("{flag} must be UTF-8")))
        };
        if matches!(
            arg.as_str(),
            "--dataset"
                | "--adjustment"
                | "--start"
                | "--end"
                | "--entities"
                | "--catalog-version"
                | "--compression"
                | "--row-group-rows"
                | "--dry-run"
        ) {
            selection_flag.get_or_insert_with(|| arg.clone());
        }
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--list-snapshots" => list_requested = true,
            "--acquire-db" => acquire_db = Some(PathBuf::from(value("--acquire-db")?)),
            "--db" => db = Some(PathBuf::from(value("--db")?)),
            "--out" => out_root = PathBuf::from(value("--out")?),
            "--dataset" => {
                let text = value("--dataset")?;
                dataset = DatasetSelection::parse(&text)
                    .ok_or_else(|| invalid(format!("unknown --dataset {text}")))?;
            }
            "--adjustment" => {
                let text = value("--adjustment")?;
                adjustment = AdjustmentSelection::parse(&text)
                    .ok_or_else(|| invalid(format!("unknown --adjustment {text}")))?;
            }
            "--start" => start = Some(date("--start", &value("--start")?)?),
            "--end" => end = Some(date("--end", &value("--end")?)?),
            "--entities" => {
                let text = value("--entities")?;
                if catalog_version_seen {
                    return Err(invalid("--entities and --catalog-version are exclusive"));
                }
                let list: Vec<String> = text
                    .split(',')
                    .map(str::trim)
                    .filter(|entity| !entity.is_empty())
                    .map(str::to_string)
                    .collect();
                if list.is_empty() {
                    return Err(invalid("--entities names no entity"));
                }
                entity_list_seen = true;
                entities = EntitySelection::Explicit(list);
            }
            "--catalog-version" => {
                if entity_list_seen {
                    return Err(invalid("--entities and --catalog-version are exclusive"));
                }
                catalog_version_seen = true;
                entities = EntitySelection::CatalogVersion(value("--catalog-version")?)
            }
            "--compression" => {
                let text = value("--compression")?;
                compression = Compression::parse(&text)
                    .ok_or_else(|| invalid(format!("unknown --compression {text}")))?;
            }
            "--row-group-rows" => {
                let text = value("--row-group-rows")?;
                row_group_rows = text
                    .parse::<usize>()
                    .ok()
                    .filter(|rows| *rows > 0)
                    .ok_or_else(|| {
                        invalid(format!("--row-group-rows {text} is not a positive integer"))
                    })?;
            }
            "--dry-run" => dry_run_requested = true,
            "--quiet" => quiet = true,
            other => return Err(invalid(format!("unknown argument {other}"))),
        }
    }

    if list_requested {
        if let Some(flag) = selection_flag {
            return Err(invalid(format!(
                "--list-snapshots only lists; {flag} belongs to an export"
            )));
        }
        return list(&out_root, db.as_deref(), acquire_db, quiet);
    }
    if acquire_db.is_some() {
        return Err(invalid("--acquire-db belongs to --list-snapshots"));
    }

    let db = db.ok_or_else(|| invalid(format!("--db is required\n{USAGE}")))?;
    let request = ExportRequest {
        dataset,
        adjustment,
        start,
        end,
        entities,
    };
    let options = ExportOptions {
        out_root,
        compression,
        row_group_rows,
    };

    if dry_run_requested {
        let (plan, manifest) = dry_run(&db, &request, &options).map_err(|e| e.to_string())?;
        if !quiet {
            let text = serde_json::to_string_pretty(&manifest)
                .map_err(|e| invalid(format!("cannot render the manifest: {e}")))?;
            println!("{text}");
            eprintln!(
                "{} files planned, no file written (--dry-run)",
                plan.files.len()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }

    let outcome = export(&db, &request, &options).map_err(|e| e.to_string())?;
    if !quiet {
        let total_bytes: u64 = outcome.files.iter().map(|file| file.bytes).sum();
        let summary = json!({
            "snapshot_id": outcome.snapshot_id,
            "directory": outcome.directory.display().to_string(),
            "files": outcome
                .files
                .iter()
                .map(|file| json!({
                    "path": file.path,
                    "rows": file.rows,
                    "bytes": file.bytes,
                    "sha256": file.sha256,
                }))
                .collect::<Vec<_>>(),
            "total_bytes": total_bytes,
            "started_at": outcome.started_at,
            "finished_at": outcome.finished_at,
            "wal_bytes_delta": outcome.wal_bytes_delta,
        });
        let text = serde_json::to_string(&summary)
            .map_err(|e| invalid(format!("cannot render the summary: {e}")))?;
        println!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The generation states come from the acquire store: `--acquire-db`, else the
/// `generations/acquire.sqlite` next to `--db` (the deployed layout keeps both
/// under `state/`), else unknown.
fn list(
    out_root: &std::path::Path,
    db: Option<&std::path::Path>,
    acquire_db: Option<PathBuf>,
    quiet: bool,
) -> Result<ExitCode, String> {
    let acquire_db = acquire_db.or_else(|| {
        db.and_then(|db| db.parent())
            .map(|dir| dir.join("generations").join("acquire.sqlite"))
    });
    let states = match &acquire_db {
        Some(path) => GenerationStates::load(path),
        None => GenerationStates::NotConfigured,
    };
    let listing =
        list_snapshots(out_root, acquire_db.as_deref(), &states).map_err(|e| e.to_string())?;
    let text = serde_json::to_string_pretty(&listing.to_json())
        .map_err(|e| invalid(format!("cannot render the listing: {e}")))?;
    println!("{text}");
    if !quiet {
        eprint!("{}", listing.to_text());
    }
    Ok(if listing.problem_count() == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    })
}
