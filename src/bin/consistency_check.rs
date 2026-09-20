use std::{fs::File, io::Read};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use x_data_mcp::file_consistency::{
    ByteIntegrity, CompatibilityEvidence, ConsistencyInput, FileFact,
};
use x_data_mcp::snapshot::FileRole;

/// Input schema is local to this diagnostic binary — the contract types
/// carry no Deserialize of their own on purpose.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    files: Vec<InputFile>,
    compatibility: Option<CompatibilityEvidence>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFile {
    role: FileRole,
    bytes: u64,
    sha256: String,
    source_url: String,
    acquisition_started_at: DateTime<Utc>,
    acquisition_completed_at: DateTime<Utc>,
    last_modified: Option<String>,
    etag: Option<String>,
    byte_integrity: ByteIntegrity,
    source_revision: Option<String>,
    source_identity: Option<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 1 {
        return Err(
            "usage: consistency_check <input.json> — evaluates \
             file_consistency::validate and prints the verbatim decision"
                .into(),
        );
    }
    let mut bytes = Vec::new();
    File::open(&args[0])?.read_to_end(&mut bytes)?;
    let input: Input = serde_json::from_slice(&bytes)?;
    let decision = x_data_mcp::file_consistency::validate(ConsistencyInput {
        files: input
            .files
            .into_iter()
            .map(|f| FileFact {
                role: f.role,
                bytes: f.bytes,
                sha256: f.sha256,
                source_url: f.source_url,
                acquisition_started_at: f.acquisition_started_at,
                acquisition_completed_at: f.acquisition_completed_at,
                last_modified: f.last_modified,
                etag: f.etag,
                byte_integrity: f.byte_integrity,
                source_revision: f.source_revision,
                source_identity: f.source_identity,
            })
            .collect(),
        compatibility: input.compatibility,
    });
    println!("{}", serde_json::to_string_pretty(&decision)?);
    Ok(())
}
