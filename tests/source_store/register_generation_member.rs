//! `source_store register-generation-member` (XDM-150, scheme A of the
//! legacy-generation retirement report §4.3/§8.0).
//!
//! A generation acquired before the source store existed has its
//! `tdxhy.cfg`/`incon.dat` bytes in the store under `archive_import`, but the
//! retirement gate looks for `generation:<manifest sha>` provenance and refuses
//! it (`source_store_evidence_missing`). These tests pin the repair command's
//! contract against synthetic generation directories (tempdir only, no
//! network, no live store): the evidence becomes real, a repeated run changes
//! nothing, mismatched inputs are refused without touching the store, an
//! existing blob is reused instead of rewritten, and after
//! `replay --scope generation_facts` the gate is left with exactly the three
//! market-derived facts such a generation never wrote.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use x_data_mcp::generation_gate::{self, FactGap, RetireGate};
use x_data_mcp::source_store::{Registration, SourceStore, StoreKind};
use x_data_mcp::sqlite_tasks::SqliteTasks;

/// One synthetic installed generation: the three role archives plus the
/// manifest whose digest names it.
struct Generation {
    _dir: tempfile::TempDir,
    dir: PathBuf,
    generation: String,
    base_zip_sha256: String,
    gbbq_zip_sha256: String,
    tdxhy: Vec<u8>,
}

const TDXHY: &[u8] = b"1|600006|T090302|||X6102\r\n1|600007|T090302|||X6102\r\n";
const INCON: &[u8] = b"#TDXRSHY\r\nX6102|RESEARCH-6102\r\n######\r\n";

fn sha(bytes: &[u8]) -> String {
    x_data_mcp::hash::sha256(bytes)
}

fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut writer = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap();
}

fn zip_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        writer.start_file(*name, options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// `base.zip!tdxhy.cfg` present or absent, `gbbq.zip!zhb.zip!incon.dat` always
/// present (the dictionary only matters once `tdxhy.cfg` exists).
fn generation(base_members: &[(&str, &[u8])]) -> Generation {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let zhb = zip_bytes(&[("incon.dat", INCON)]);
    let roles = [
        (
            "base_industry",
            "base.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/base.zip",
            base_members.to_vec(),
        ),
        (
            "day",
            "hsjday.zip",
            "https://data.tdx.com.cn/vipdoc/hsjday.zip",
            Vec::new(),
        ),
        (
            "gbbq",
            "gbbq.zip",
            "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip",
            vec![("zhb.zip", zhb.as_slice())],
        ),
    ];
    let mut files = Vec::new();
    let mut digests = BTreeMap::new();
    for (role, name, url, members) in roles {
        let path = root.join(name);
        write_zip(&path, &members);
        let bytes = std::fs::read(&path).unwrap();
        let digest = sha(&bytes);
        digests.insert(name, digest.clone());
        files.push(json!({
            "role": role,
            "relative_path": name,
            "source_url": url,
            "acquisition_started_at": "2026-10-19T01:00:00Z",
            "acquisition_completed_at": "2026-10-19T02:00:00Z",
            "last_modified": "2026-10-19T00:00:00Z",
            "etag": null,
            "bytes": bytes.len(),
            "sha256": digest,
        }));
    }
    let manifest =
        serde_json::to_vec(&json!({"schema":"x-data-mcp.official-file-input.v1","files":files}))
            .unwrap();
    std::fs::write(root.join("snapshot-manifest.json"), &manifest).unwrap();
    Generation {
        generation: sha(&manifest),
        base_zip_sha256: digests["base.zip"].clone(),
        gbbq_zip_sha256: digests["gbbq.zip"].clone(),
        tdxhy: TDXHY.to_vec(),
        dir: root,
        _dir: dir,
    }
}

fn with_tdxhy() -> Generation {
    generation(&[("tdxhy.cfg", TDXHY), ("base.dbf", b"unused")])
}

fn register(store: &Path, generation: &str, dir: &Path) -> (i32, serde_json::Value) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_source_store"))
        .args([
            "register-generation-member",
            "--store",
            store.to_str().unwrap(),
            "--generation",
            generation,
            "--generation-dir",
            dir.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "register-generation-member printed no JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap_or(-1), report)
}

fn replay_facts(store: &Path, db: &Path) -> (i32, serde_json::Value) {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_source_store"))
        .args([
            "replay",
            "--store",
            store.to_str().unwrap(),
            "--db",
            db.to_str().unwrap(),
            "--scope",
            "generation_facts",
        ])
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "replay printed no JSON ({error}); stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().unwrap_or(-1), report)
}

fn market_db(dir: &Path) -> PathBuf {
    let db = dir.join("market.db");
    SqliteTasks::new(&db).initialize_compact().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    db
}

fn lines(store: &SourceStore) -> Vec<x_data_mcp::source_store::ManifestRecord> {
    store.records().unwrap()
}

fn blobs(store: &SourceStore) -> usize {
    std::fs::read_dir(store.blobs_dir())
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn fact_keys(gaps: &[FactGap]) -> Vec<String> {
    gaps.iter().map(FactGap::key).collect()
}

/// The retroactive registration turns "same bytes, wrong provenance" into real
/// evidence; a repeated run writes nothing, and after the fact-index replay the
/// gate is left with only the three market-derived facts the legacy generation
/// never wrote.
#[test]
fn registration_makes_the_evidence_real_and_leaves_only_market_facts() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = with_tdxhy();

    // The legacy state: the same member bytes already in the store, under the
    // owner's archive-import origin. The gate cannot see them.
    let mut imported = Registration::new(
        x_data_mcp::source_store::TDXHY_SOURCE_ID,
        StoreKind::ArchiveImport,
        "owner-archive-20261028:container!tdxhy.cfg",
        "2026-10-31T18:26:29Z",
    );
    imported.parser_version = Some(x_data_mcp::source_store::TDXHY_PARSER_VERSION.to_string());
    let imported_outcome = store.register(&generation.tdxhy, &imported).unwrap();
    assert!(imported_outcome.written_blob && imported_outcome.appended_line);
    assert_eq!(lines(&store).len(), 1);
    assert_eq!(blobs(&store), 1);

    let db = market_db(root.path());
    let before = RetireGate::new(store.clone(), db.clone())
        .check(&generation.generation)
        .unwrap();
    assert!(
        !before.missing_evidence.is_empty(),
        "before the backfill the gate must refuse the evidence section"
    );
    assert_eq!(
        before.reason_code(),
        Some("source_store_evidence_missing"),
        "{before:?}"
    );

    let (code, report) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["status"], "ok");
    assert_eq!(
        report["origin"],
        format!("generation:{}", generation.generation).as_str()
    );
    let members = report["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["role"], "base.zip");
    assert_eq!(
        members[0]["source_id"],
        x_data_mcp::source_store::TDXHY_SOURCE_ID
    );
    assert_eq!(members[0]["member"], "tdxhy.cfg");
    assert_eq!(members[0]["sha256"], sha(&generation.tdxhy).as_str());
    assert_eq!(
        members[0]["size_bytes"].as_u64(),
        Some(generation.tdxhy.len() as u64)
    );
    assert_eq!(members[0]["added"], true);
    assert_eq!(
        members[0]["blob_reused"], true,
        "the archive-import blob is reused, not rewritten"
    );
    assert_eq!(members[1]["role"], "gbbq.zip");
    assert_eq!(
        members[1]["source_id"],
        x_data_mcp::source_store::INCON_SOURCE_ID
    );
    assert_eq!(members[1]["member"], "zhb.zip!incon.dat");
    assert_eq!(members[1]["sha256"], sha(INCON).as_str());
    assert_eq!(members[1]["added"], true);
    assert_eq!(
        members[1]["blob_reused"], false,
        "the dictionary blob was not in the store yet"
    );

    // One extra blob (the dictionary), two extra lines (one per member); the
    // existing `tdxhy.cfg` line was not modified.
    assert_eq!(blobs(&store), 2);
    let records = lines(&store);
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].origin, imported.origin);
    for record in &records[1..] {
        assert_eq!(
            record.origin,
            format!("generation:{}", generation.generation)
        );
        assert_eq!(record.store_kind, StoreKind::GenerationMember);
        assert_eq!(record.evidence_grade.as_deref(), Some("A"));
        assert_eq!(
            record.publisher_last_modified.as_deref(),
            Some("2026-10-19T00:00:00Z")
        );
    }

    // The evidence section passes with no exemption; only derived facts remain.
    let after = generation_gate::evaluate(&store, &generation.generation, &[], &[]).unwrap();
    assert!(after.missing_evidence.is_empty(), "{after:?}");
    assert_eq!(
        after.missing_facts.len(),
        6,
        "the index has not been rebuilt"
    );
    assert_eq!(after.reason_code(), Some("derived_facts_missing"));

    let (code, replay) = replay_facts(store.root(), &db);
    assert_eq!(code, 0, "{replay}");
    assert_eq!(replay["status"], "ok");
    assert_eq!(replay["scopes"][0]["rebuilt"], "rebuilt");
    assert_eq!(replay["scopes"][0]["database"], "match");
    assert_eq!(replay["scopes"][0]["facts"], 3);

    let gate = RetireGate::new(store.clone(), db)
        .check(&generation.generation)
        .unwrap();
    assert!(gate.missing_evidence.is_empty(), "{gate:?}");
    assert_eq!(
        fact_keys(&gate.missing_facts),
        vec![
            "derived_fact:gbbq.zip:capital_events",
            "derived_fact:gbbq.zip:adjustment_events",
            "derived_fact:hsjday.zip:daily_series",
        ]
    );
    assert_eq!(gate.reason_code(), Some("derived_facts_missing"));
}

/// A repeated run appends no line and writes no blob: the command is safe to
/// retry inside the retirement window.
#[test]
fn repeated_run_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = with_tdxhy();

    let (code, first) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 0, "{first}");
    let blob_count = blobs(&store);
    let line_count = lines(&store).len();

    let (code, second) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 0, "{second}");
    for member in second["members"].as_array().unwrap() {
        assert_eq!(member["added"], false, "no line was appended: {member}");
        assert_eq!(member["blob_reused"], true, "{member}");
    }
    assert_eq!(blobs(&store), blob_count);
    assert_eq!(lines(&store).len(), line_count);
    assert_eq!(
        second["members"][0]["sha256"],
        first["members"][0]["sha256"]
    );
    assert_eq!(
        second["members"][1]["sha256"],
        first["members"][1]["sha256"]
    );
}

/// `--generation` must be the digest of the directory's manifest; anything else
/// is refused before the store is touched.
#[test]
fn rejects_a_generation_that_is_not_the_manifest_digest() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = with_tdxhy();
    let wrong = "a".repeat(64);

    let (code, report) = register(store.root(), &wrong, &generation.dir);
    assert_eq!(code, 1);
    assert_eq!(report["status"], "failed");
    assert_eq!(
        report["code"],
        x_data_mcp::source_store::generation_member::MANIFEST_MISMATCH
    );
    let detail = report["detail"].as_str().unwrap();
    assert!(detail.contains(&wrong), "{detail}");
    assert!(detail.contains(&generation.generation), "{detail}");
    assert!(
        !store.root().exists(),
        "a refused run must not create the store"
    );
}

/// An archive whose bytes no longer match their manifest pin is refused: the
/// container digest is written into the manifest line as evidence, so it has to
/// be recomputed, not trusted.
#[test]
fn rejects_a_container_that_no_longer_matches_its_manifest_pin() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = with_tdxhy();
    // Rewrite `base.zip` after the manifest was sealed.
    write_zip(
        &generation.dir.join("base.zip"),
        &[("tdxhy.cfg", TDXHY), ("base.dbf", b"tampered")],
    );

    let (code, report) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 1);
    assert_eq!(
        report["code"],
        x_data_mcp::source_store::generation_member::CONTAINER_MISMATCH
    );
    assert!(
        report["detail"].as_str().unwrap().contains("base.zip"),
        "{report}"
    );
    assert!(
        !store.root().exists(),
        "a refused run must not create the store"
    );
}

/// A generation that ships no `tdxhy.cfg` has nothing to register and the gate
/// would still refuse it, so the command reports a failure instead of `ok`.
#[test]
fn rejects_a_generation_without_an_industry_member() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = generation(&[("base.dbf", b"no industry member here")]);

    let (code, report) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 1);
    assert_eq!(
        report["code"],
        x_data_mcp::source_store::generation_member::NO_INDUSTRY_MEMBER
    );
    assert!(!store.root().exists());
}

/// Each manifest line names the container its member actually came from:
/// `tdxhy.cfg` → `base.zip`, `incon.dat` → `gbbq.zip` (XDM-150 fixed the
/// `incon.dat` line, which used to carry `base.zip`'s digest).
#[test]
fn writes_each_members_own_container_digest() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::new(root.path().join("store"));
    let generation = with_tdxhy();
    let (code, report) = register(store.root(), &generation.generation, &generation.dir);
    assert_eq!(code, 0, "{report}");

    let records = lines(&store);
    let tdxhy = records
        .iter()
        .find(|record| record.source_id == x_data_mcp::source_store::TDXHY_SOURCE_ID)
        .unwrap();
    let incon = records
        .iter()
        .find(|record| record.source_id == x_data_mcp::source_store::INCON_SOURCE_ID)
        .unwrap();
    assert_eq!(
        tdxhy.container_sha256.as_deref(),
        Some(generation.base_zip_sha256.as_str())
    );
    assert_eq!(
        incon.container_sha256.as_deref(),
        Some(generation.gbbq_zip_sha256.as_str()),
        "the dictionary line must name gbbq.zip, not base.zip"
    );
    assert_ne!(generation.base_zip_sha256, generation.gbbq_zip_sha256);
}
