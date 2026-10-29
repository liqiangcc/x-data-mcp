//! AC-A02 crash-point fault injection for the acquire service (#260/#262
//! DoD-3). Each test reconstructs the exact disk/DB state a crash leaves
//! at one persistence boundary, reopens, and asserts: the terminal state
//! is mechanically determinable and unique; a previous active generation
//! keeps its bytes; residue is either absent or deterministically
//! adopted/discarded; and a second `open` changes nothing.
//!
//! Boundaries enumerated from `src/acquire/service.rs` (`advance`,
//! `verify_now`, `activate_now`, `retire_now`) and
//! `src/generation/mod.rs` (`install_staged`, `activate`, `retire_bytes`)
//! — see RESULT-PAR-2.md for the full crash-point table.
use crate::acquire_service::{
    activate, drive, gate_fixture, generation_of, install, open, open_gated, remirror,
    satisfy_gate, sha, today_u32, verify, write_mirror,
};
use chrono::Utc;
use std::path::Path;

fn generation_dir(root: &Path, generation_id: &str) -> std::path::PathBuf {
    root.join("generations").join(generation_id)
}

/// Directory bytes fingerprint for "previous generation byte-identical"
/// assertions: sorted (name, sha256) of every file under `dir`.
fn dir_digest(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    collect_digests(dir, dir, &mut out);
    out.sort();
    out
}
fn collect_digests(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if entry.file_type().unwrap().is_dir() {
            collect_digests(base, &path, out);
        } else {
            out.push((
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                sha(&std::fs::read(&path).unwrap()),
            ));
        }
    }
}

/// P13/P14: crash between the atomic pointer rename and the generations-
/// row update (`service.rs:902-920`, `generation/mod.rs:519-526`). The
/// on-disk pointer must win: B becomes the single `active` row, A demotes
/// to `superseded`, and neither generation loses bytes.
#[test]
fn crash_between_pointer_swap_and_row_realign() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    let gen_a = generation_of(&service, &install(&service, "req-a").task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-b").task_id);
    verify(&service, &gen_b);
    let before_a = dir_digest(&generation_dir(root.path(), &gen_a));
    let before_b = dir_digest(&generation_dir(root.path(), &gen_b));

    // Crash point: perform exactly the pointer swap `activate_now` does at
    // `store.activate` but skip the generations-row loop that follows it.
    let installed_b = service
        .generations()
        .verify_installed(&gen_b, Utc::now())
        .unwrap();
    service
        .generations()
        .activate(&installed_b, Some(&gen_a), "activate-b-crash", Utc::now())
        .unwrap();
    // Post-crash state: pointer file = B, rows still A=active /
    // B=consistency_verified.
    drop(service);

    let service = open(root.path(), mirror.path());
    let status = service.sync_status().unwrap();
    assert_eq!(
        status.active.as_ref().unwrap().generation_id,
        gen_b,
        "on-disk pointer must remain the active source of truth"
    );
    let state_of = |id: &str| {
        status
            .generations
            .iter()
            .find(|g| g.generation_id == id)
            .map(|g| g.state.clone())
    };
    assert_eq!(state_of(&gen_b).as_deref(), Some("active"));
    assert_eq!(state_of(&gen_a).as_deref(), Some("superseded"));
    assert_eq!(
        status
            .generations
            .iter()
            .filter(|g| g.state == "active")
            .count(),
        1
    );
    // The superseded generation keeps its exact bytes (no cleanup, no
    // rewrite); B is untouched as well.
    assert_eq!(dir_digest(&generation_dir(root.path(), &gen_a)), before_a);
    assert_eq!(dir_digest(&generation_dir(root.path(), &gen_b)), before_b);
    assert!(
        service.generations().usable(&gen_a, Utc::now()).is_ok(),
        "superseded generation must stay byte-usable for held sessions"
    );
    drop(service);

    // Repeated open is idempotent: nothing flips back or advances.
    let service = open(root.path(), mirror.path());
    let again = service.sync_status().unwrap();
    assert_eq!(again.active.as_ref().unwrap().generation_id, gen_b);
    let state_of = |id: &str| {
        again
            .generations
            .iter()
            .find(|g| g.generation_id == id)
            .map(|g| g.state.clone())
    };
    assert_eq!(state_of(&gen_b).as_deref(), Some("active"));
    assert_eq!(state_of(&gen_a).as_deref(), Some("superseded"));
}

// ---------- helpers for crash-state construction ----------

/// The durable acquisition store inside a store root (read/write rows
/// directly to assemble a crash-instant state, then reopen the service).
fn db(root: &Path) -> x_data_mcp::acquire::store::Db {
    let db = x_data_mcp::acquire::store::Db::new(root.join("acquire.sqlite"));
    db.open().unwrap();
    db
}

fn task_row(
    db: &x_data_mcp::acquire::store::Db,
    task_id: &str,
) -> x_data_mcp::acquire::store::Task {
    db.task(task_id).unwrap().expect("task row exists")
}

fn mark_task(
    db: &x_data_mcp::acquire::store::Db,
    task_id: &str,
    state: &str,
    stage: &str,
    generation_id: Option<&str>,
) {
    let task = task_row(db, task_id);
    db.update_task(&x_data_mcp::acquire::store::Task {
        state: state.into(),
        generation_id: generation_id.map(str::to_string),
        stage: Some(stage.into()),
        updated_at: Utc::now(),
        ..task
    })
    .unwrap();
}

fn upsert_generation_row(
    db: &x_data_mcp::acquire::store::Db,
    generation_id: &str,
    state: &str,
    manifest_sha256: &str,
    acquisition_request_id: Option<&str>,
) {
    db.upsert_generation(&x_data_mcp::acquire::store::Generation {
        generation_id: generation_id.into(),
        state: state.into(),
        manifest_sha256: manifest_sha256.into(),
        consistency_status: None,
        consistency_reasons: None,
        decision_sha256: None,
        evidence_json: None,
        acquisition_request_id: acquisition_request_id.map(str::to_string),
        installed_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .unwrap();
}

fn stage_dir(root: &Path, request_id: &str) -> std::path::PathBuf {
    root.join("staging").join(request_id)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn generation_state(
    status: &x_data_mcp::acquire::wire::StatusView,
    generation_id: &str,
) -> Option<String> {
    status
        .generations
        .iter()
        .find(|g| g.generation_id == generation_id)
        .map(|g| g.state.clone())
}

/// Drive the staged fetch pipeline up to (but not including) the named
/// commit: after `n` worker turns the task row sits at the given stage.
fn drive_stages(service: &x_data_mcp::acquire::AcquireService, n: usize) {
    for _ in 0..n {
        service.worker_once().unwrap();
    }
}

// ---------- acquisition pipeline boundaries ----------

/// P1: crash between `accept_request` and `create_task`
/// (`service.rs:427-474`). The request id stays durably bound with no
/// task behind it; the only mechanical outcome is a deterministic
/// refusal, and a fresh request id proceeds cleanly.
#[test]
fn crash_between_accept_and_task_row_leaves_bound_request() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let db = db(root.path());

    let (request, reused) = db
        .accept_request("req-p1", "acquire_generation", "{}", Utc::now())
        .unwrap();
    assert!(!reused);
    assert_eq!(request.request_id, "req-p1");

    // The bound-but-taskless request replays to a deterministic refusal —
    // it can never bind a task under this id.
    let first = service.acquire_generation("req-p1").unwrap_err();
    assert_eq!(first.code(), "request_binding_lost");
    let again = service.acquire_generation("req-p1").unwrap_err();
    assert_eq!(again.code(), "request_binding_lost");
    let view = service.request("req-p1").unwrap();
    assert!(view.bound);
    assert!(view.task.is_none());
    assert!(view.terminal_result.is_none());

    // A different request id is unaffected: the pipeline still works.
    let acceptance = install(&service, "req-p1-fresh");
    assert_eq!(
        service.task(&acceptance.task_id).unwrap().state,
        "succeeded"
    );
    assert_eq!(service.sync_status().unwrap().generations.len(), 1);
}

/// P2: crash between `create_task` and `create_stage`
/// (`service.rs:474-480`). The staged download area never existed; the
/// resumed fetch fails closed and records a durable failed outcome.
#[test]
fn crash_between_task_row_and_stage_dir_fails_closed() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let db = db(root.path());
    db.accept_request("req-p2", "acquire_generation", "{}", Utc::now())
        .unwrap();
    db.create_task(&x_data_mcp::acquire::store::Task {
        task_id: "acq-p2".into(),
        request_id: "req-p2".into(),
        kind: "download_install".into(),
        state: "accepted".into(),
        generation_id: None,
        stage: Some("fetch:base_industry".into()),
        bytes_done: 0,
        error: None,
        detail: Some("{\"files\":{}}".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    service.worker_once().unwrap();
    let task = service.task("acq-p2").unwrap();
    assert_eq!(task.state, "failed", "{task:?}");
    assert!(task.error.is_some());
    let view = service.request("req-p2").unwrap();
    assert!(view.terminal_result.is_some());
    // Nothing half-installed, no stage residue, next worker turn idles.
    assert!(service.generations().installed_ids().unwrap().is_empty());
    assert!(!stage_dir(root.path(), "req-p2").exists());
    assert!(service.worker_once().unwrap().is_none());
}

/// P3+P4: crash with task `running` mid-fetch. Reconcile returns it to
/// `accepted` and the resume keeps already-pinned staged bytes — proven
/// by deleting the mirror copies of the roles already fetched.
#[test]
fn crash_mid_fetch_recovers_and_resumes_with_pinned_bytes() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let acceptance = service.acquire_generation("req-p3").unwrap();

    // Two committed stages: base_industry + gbbq fetched and pinned in
    // the task detail; the row says running at fetch:day.
    drive_stages(&service, 2);
    assert_eq!(
        task_row(&db(root.path()), &acceptance.task_id)
            .stage
            .as_deref(),
        Some("fetch:day")
    );
    // Remove the mirror sources for the roles already committed — a
    // correct resume never fetches them again.
    std::fs::remove_file(mirror.path().join("base.zip")).unwrap();
    std::fs::remove_file(mirror.path().join("gbbq.zip")).unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    // running -> accepted is the mechanically recorded transition.
    assert_eq!(service.task(&acceptance.task_id).unwrap().state, "accepted");
    drive(&service);
    let task = service.task(&acceptance.task_id).unwrap();
    assert_eq!(task.state, "succeeded", "{task:?}");
    let generation_id = task.generation_id.unwrap();
    assert!(generation_dir(root.path(), &generation_id).is_dir());
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &generation_id).as_deref(),
        Some("byte_verified")
    );
}

/// P5: crash leaves a staged artifact that no longer matches its
/// manifest pin (torn write). Install byte-verification is the
/// fail-closed gate: the task fails, nothing installs.
#[test]
fn crash_with_corrupt_staged_artifact_fails_closed_at_install() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let acceptance = service.acquire_generation("req-p5").unwrap();
    drive_stages(&service, 4); // through manifest; stage=install
    assert_eq!(
        task_row(&db(root.path()), &acceptance.task_id)
            .stage
            .as_deref(),
        Some("install")
    );
    // Tear a staged artifact at the crash instant.
    std::fs::write(
        stage_dir(root.path(), "req-p5").join("base.zip"),
        b"torn-write",
    )
    .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    service.worker_once().unwrap();
    let task = service.task(&acceptance.task_id).unwrap();
    assert_eq!(task.state, "failed", "{task:?}");
    assert!(service.generations().installed_ids().unwrap().is_empty());
    assert!(service.request("req-p5").unwrap().terminal_result.is_some());
}

/// P6/P7: crashes either side of `write_manifest` inside the `manifest`
/// stage (`service.rs:557-568`). The absent manifest is (re)written and
/// an already-written manifest is rewritten byte-identically; both
/// converge on the same install.
#[test]
fn crash_around_manifest_stage_resumes_to_identical_install() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    // Variant A: stage=manifest committed, manifest file absent.
    let a = service.acquire_generation("req-p6a").unwrap();
    drive_stages(&service, 3);
    assert_eq!(
        task_row(&db(root.path()), &a.task_id).stage.as_deref(),
        Some("manifest")
    );
    drop(service);
    let service = open(root.path(), mirror.path());
    drive(&service);
    let task_a = service.task(&a.task_id).unwrap();
    assert_eq!(task_a.state, "succeeded");
    let gen_a = task_a.generation_id.unwrap();
    let manifest_a =
        std::fs::read(generation_dir(root.path(), &gen_a).join("snapshot-manifest.json")).unwrap();

    // Variant B: crash after write_manifest but before the stage row
    // advanced — manifest exists while the row still says `manifest`.
    let b = service.acquire_generation("req-p6b").unwrap();
    drive_stages(&service, 3);
    let detail = task_row(&db(root.path()), &b.task_id)
        .detail
        .expect("task detail");
    let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
    let files: Vec<x_data_mcp::snapshot::FileInput> = ["base_industry", "gbbq", "day"]
        .iter()
        .map(|key| serde_json::from_value(detail["files"][*key].clone()).unwrap())
        .collect();
    let manifest_b = serde_json::to_vec_pretty(&x_data_mcp::snapshot::SnapshotManifest {
        schema: "x-data-mcp.official-file-input.v1".into(),
        files,
    })
    .unwrap();
    std::fs::write(
        stage_dir(root.path(), "req-p6b").join("snapshot-manifest.json"),
        &manifest_b,
    )
    .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    drive(&service);
    let task_b = service.task(&b.task_id).unwrap();
    assert_eq!(task_b.state, "succeeded");
    let gen_b = task_b.generation_id.unwrap();
    // The manifest the service rewrote is byte-identical to the one the
    // crash left behind: generation identity is deterministic.
    assert_eq!(
        sha(&manifest_b),
        gen_b,
        "generation id is the manifest digest"
    );
    assert_eq!(
        std::fs::read(generation_dir(root.path(), &gen_b).join("snapshot-manifest.json")).unwrap(),
        manifest_b
    );
    // Both variants installed real, distinct generations.
    assert_ne!(gen_a, gen_b);
    assert_eq!(service.sync_status().unwrap().generations.len(), 2);
    let parsed: x_data_mcp::snapshot::SnapshotManifest =
        serde_json::from_slice(&manifest_a).unwrap();
    assert_eq!(parsed.files.len(), 3);
}

/// P8: crash between the `install_staged` rename and `upsert_generation`
/// (`service.rs:571-583`). The generation directory exists with no row:
/// reconcile adopts it `byte_verified`; the task — whose stage row still
/// says `install` — fails deterministically because its staging area is
/// gone. The adopted bytes remain verifiable and activatable.
#[test]
fn crash_between_install_rename_and_row_insert_adopts_orphan() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let a = service.acquire_generation("req-p8").unwrap();
    drive_stages(&service, 4);
    let gen_id = sha(&std::fs::read(
        stage_dir(root.path(), "req-p8").join("snapshot-manifest.json"),
    )
    .unwrap());
    std::fs::rename(
        stage_dir(root.path(), "req-p8"),
        generation_dir(root.path(), &gen_id),
    )
    .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    // Adoption: the orphan dir gains a byte_verified row at open.
    let status = service.sync_status().unwrap();
    assert_eq!(
        generation_state(&status, &gen_id).as_deref(),
        Some("byte_verified"),
        "{status:?}"
    );
    // The resumed task cannot reinstall: stage dir is gone.
    service.worker_once().unwrap();
    let task = service.task(&a.task_id).unwrap();
    assert_eq!(task.state, "failed", "{task:?}");
    let replay = service.acquire_generation("req-p8").unwrap();
    assert!(replay.reused);
    assert_eq!(replay.state, "failed");
    // The adopted generation is a normal byte-verified candidate.
    let reply = verify(&service, &gen_id);
    assert_eq!(reply.state, "consistency_verified", "{reply:?}");
    assert_eq!(activate(&service, &gen_id, None).state, "active");
}

/// P9: same boundary with the generations row already committed
/// (`service.rs:583-584` vs `:584-593`). Identical mechanical outcome:
/// task fails, the row keeps the installed generation.
#[test]
fn crash_between_generation_row_and_task_completion() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let a = service.acquire_generation("req-p9").unwrap();
    drive_stages(&service, 4);
    let gen_id = sha(&std::fs::read(
        stage_dir(root.path(), "req-p9").join("snapshot-manifest.json"),
    )
    .unwrap());
    std::fs::rename(
        stage_dir(root.path(), "req-p9"),
        generation_dir(root.path(), &gen_id),
    )
    .unwrap();
    let installed = service
        .generations()
        .verify_installed(&gen_id, Utc::now())
        .unwrap();
    upsert_generation_row(
        &db(root.path()),
        &gen_id,
        "byte_verified",
        &installed.manifest_sha256,
        Some("req-p9"),
    );
    drop(service);

    let service = open(root.path(), mirror.path());
    service.worker_once().unwrap();
    assert_eq!(service.task(&a.task_id).unwrap().state, "failed");
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_id).as_deref(),
        Some("byte_verified")
    );
    assert!(generation_dir(root.path(), &gen_id).is_dir());
    drop(service);
    // Reopen is idempotent: no flapping between adopt/fail paths.
    let service = open(root.path(), mirror.path());
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_id).as_deref(),
        Some("byte_verified")
    );
    assert_eq!(service.task(&a.task_id).unwrap().state, "failed");
}

/// P10: crash between `update_task(succeeded)` and `record_outcome`
/// (`service.rs:593-594`). The terminal outcome row was never written;
/// the request view and the replayed acceptance still expose the
/// succeeded task, so the state is mechanically determinable.
#[test]
fn crash_between_task_completion_and_outcome_record() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let a = service.acquire_generation("req-p10").unwrap();
    drive_stages(&service, 4);
    // Perform the install branch's first two commits by hand, stopping
    // before record_outcome.
    let installed = service
        .generations()
        .install_staged("req-p10", Utc::now())
        .unwrap();
    upsert_generation_row(
        &db(root.path()),
        &installed.generation_id,
        "byte_verified",
        &installed.manifest_sha256,
        Some("req-p10"),
    );
    mark_task(
        &db(root.path()),
        &a.task_id,
        "succeeded",
        "done",
        Some(&installed.generation_id),
    );
    drop(service);

    let service = open(root.path(), mirror.path());
    let view = service.request("req-p10").unwrap();
    assert!(view.bound);
    assert_eq!(view.task.unwrap().state, "succeeded");
    assert!(
        view.terminal_result.is_none(),
        "outcome row was lost with the crash — determinable, not fabricated"
    );
    // Replaying the request returns the recorded acceptance with the
    // terminal task state attached.
    let replay = service.acquire_generation("req-p10").unwrap();
    assert!(replay.reused);
    assert_eq!(replay.state, "succeeded");
    assert!(generation_dir(root.path(), &installed.generation_id).is_dir());
}

/// P11: crash between `record_outcome` and `remove_stage`
/// (`service.rs:594-599`). Residual staging dirs of terminal tasks are
/// download residue — reconcile removes them only now that the outcome
/// is durable. Covers both succeeded and failed tasks.
#[test]
fn crash_after_outcome_record_stage_residue_is_removed() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let a = install(&service, "req-p11a");
    assert_eq!(service.task(&a.task_id).unwrap().state, "succeeded");
    // Recreate the residue the crash left: outcome is durable (task
    // succeeded, outcome row recorded by install), stage dir reappears.
    let residue = stage_dir(root.path(), "req-p11a");
    std::fs::create_dir_all(&residue).unwrap();
    std::fs::write(residue.join("partial.zip"), b"partial").unwrap();
    // A failed task leaves residue too.
    let dbh = db(root.path());
    dbh.accept_request("req-p11b", "acquire_generation", "{}", Utc::now())
        .unwrap();
    dbh.create_task(&x_data_mcp::acquire::store::Task {
        task_id: "acq-p11b".into(),
        request_id: "req-p11b".into(),
        kind: "download_install".into(),
        state: "failed".into(),
        generation_id: None,
        stage: Some("fetch:day".into()),
        bytes_done: 0,
        error: Some("injected".into()),
        detail: Some("{\"files\":{}}".into()),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .unwrap();
    let residue_b = stage_dir(root.path(), "req-p11b");
    std::fs::create_dir_all(&residue_b).unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    assert!(!residue.exists(), "succeeded task residue must be swept");
    assert!(!residue_b.exists(), "failed task residue must be swept");
    assert_eq!(service.task(&a.task_id).unwrap().state, "succeeded");
}

// ---------- activation pointer boundaries ----------

/// P12: crash inside `Store::activate` between the tmp-file write and
/// its rename (`generation/mod.rs:519-524`). The leftover
/// `.active-generation.json.tmp` must not change the pointer: A stays
/// active, and the residue is overwritten by the next real activation.
#[test]
fn crash_with_pointer_tmp_residue_preserves_active() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let gen_a = generation_of(&service, &install(&service, "req-p12a").task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    // Crash residue: the tmp pointer file the rename never consumed.
    let tmp_pointer = root.path().join(".active-generation.json.tmp");
    std::fs::write(&tmp_pointer, b"{\"schema\":\"half-written\"").unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    assert_eq!(
        service.sync_status().unwrap().active.unwrap().generation_id,
        gen_a
    );
    assert!(
        tmp_pointer.exists(),
        "tmp residue is ignored, not deleted by open"
    );
    // The next activation runs the same tmp+rename path and consumes it.
    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-p12b").task_id);
    verify(&service, &gen_b);
    assert_eq!(
        service
            .activate_generation("activate-p12b", &gen_b, Some(&gen_a))
            .unwrap()
            .state,
        "active"
    );
    assert_eq!(
        service.sync_status().unwrap().active.unwrap().generation_id,
        gen_b
    );
    assert!(!tmp_pointer.exists());
}

/// P14: crash mid row-update after the pointer swap (`service.rs:906-920`)
/// — B was flipped to `active` but A's row is still `active` too.
/// Reconcile collapses to exactly one active row: the pointer's.
#[test]
fn crash_with_two_active_rows_collapses_to_pointer() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let gen_a = generation_of(&service, &install(&service, "req-p14a").task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-p14b").task_id);
    verify(&service, &gen_b);

    let installed_b = service
        .generations()
        .verify_installed(&gen_b, Utc::now())
        .unwrap();
    service
        .generations()
        .activate(&installed_b, Some(&gen_a), "activate-p14", Utc::now())
        .unwrap();
    // Crash mid-loop: B's row was updated, A's was not.
    upsert_generation_row(
        &db(root.path()),
        &gen_b,
        "active",
        &installed_b.manifest_sha256,
        None,
    );
    drop(service);

    let service = open(root.path(), mirror.path());
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_b);
    assert_eq!(generation_state(&status, &gen_b).as_deref(), Some("active"));
    assert_eq!(
        generation_state(&status, &gen_a).as_deref(),
        Some("superseded")
    );
    assert_eq!(
        status
            .generations
            .iter()
            .filter(|g| g.state == "active")
            .count(),
        1
    );
    // The bytes of the superseded active generation are untouched.
    assert!(service.generations().usable(&gen_a, Utc::now()).is_ok());
}

// ---------- verification boundaries ----------

/// P15: crash between `write_artifact(EVIDENCE)` and `write_decision`
/// (`service.rs:764-791`). The evidence file exists but the decision was
/// never written. Verify re-entry treats the residue as an idempotent
/// hit when its bytes match the recomputed evidence exactly: the
/// write-once store call is skipped (the file is never rewritten), the
/// decision write completes, and the generation becomes activatable.
#[test]
fn crash_between_evidence_write_and_decision_verify_reentry_recovers() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    // A healthy active generation must stay untouched throughout.
    let gen_a = generation_of(&service, &install(&service, "req-p15a").task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    let before_a = dir_digest(&generation_dir(root.path(), &gen_a));

    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-p15b").task_id);
    // Produce the real evidence bytes with one complete verify, then
    // remove only the decision file — the exact P15 disk residue
    // (evidence written, decision absent).
    verify(&service, &gen_b);
    let evidence_path = generation_dir(root.path(), &gen_b).join("compatibility-evidence.json");
    let evidence_sha = sha(&std::fs::read(&evidence_path).unwrap());
    std::fs::remove_file(generation_dir(root.path(), &gen_b).join("consistency-decision.json"))
        .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    // Re-entry completes the interrupted verify; the evidence file keeps
    // its original bytes and the decision is written fresh.
    let reply = service.verify_generation("req-p15-v1", &gen_b).unwrap();
    assert_eq!(
        reply.consistency_status.as_deref(),
        Some("VERIFIED_COMPATIBLE")
    );
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_b).as_deref(),
        Some("consistency_verified")
    );
    assert_eq!(sha(&std::fs::read(&evidence_path).unwrap()), evidence_sha);
    let decision_path = generation_dir(root.path(), &gen_b).join("consistency-decision.json");
    assert!(decision_path.exists());
    assert_eq!(
        reply.decision_sha256.as_deref(),
        Some(sha(&std::fs::read(&decision_path).unwrap()).as_str())
    );
    // A further verify under another request replays the recorded decision.
    let replayed = service.verify_generation("req-p15-v2", &gen_b).unwrap();
    assert_eq!(replayed.decision_sha256, reply.decision_sha256);
    // The recovered generation now passes the full gate and activates.
    assert_eq!(activate(&service, &gen_b, Some(&gen_a)).state, "active");
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_b);
    assert_eq!(dir_digest(&generation_dir(root.path(), &gen_a)), before_a);
}

/// P15b: same crash point, but the residue bytes differ from the
/// recomputed evidence. Verify stays fail-closed: a deterministic error,
/// the foreign bytes are never overwritten or removed, the row never
/// advances, and activation remains refused.
#[test]
fn crash_evidence_residue_mismatch_fails_closed() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    // A healthy active generation must stay untouched throughout.
    let gen_a = generation_of(&service, &install(&service, "req-p15ma").task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    let before_a = dir_digest(&generation_dir(root.path(), &gen_a));

    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-p15mb").task_id);
    // Crash residue whose bytes could not have come from this pipeline.
    let evidence_path = generation_dir(root.path(), &gen_b).join("compatibility-evidence.json");
    std::fs::write(&evidence_path, b"{}").unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    for request_id in ["req-p15m-v1", "req-p15m-v2"] {
        let err = service.verify_generation(request_id, &gen_b).unwrap_err();
        assert!(
            err.to_string().contains("different bytes"),
            "expected the byte-mismatch conflict, got {err:?}"
        );
    }
    // The residue is byte-identical afterwards: no overwrite, no delete.
    assert_eq!(std::fs::read(&evidence_path).unwrap(), b"{}");
    // The row never advances; activation fails closed.
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_b).as_deref(),
        Some("byte_verified")
    );
    let refused = service
        .activate_generation("activate-p15mb", &gen_b, Some(&gen_a))
        .unwrap();
    assert_eq!(refused.state, "refused");
    assert_eq!(
        refused.reason_code.as_deref(),
        Some("generation_not_consistency_verified")
    );
    // The previous active generation is byte-identical and still active.
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_a);
    assert_eq!(dir_digest(&generation_dir(root.path(), &gen_a)), before_a);
}

/// P16: crash between `write_decision` and `upsert_generation`
/// (`service.rs:791-817`). The decision artifact exists while the row
/// still says `byte_verified`. The artifact is authoritative: `usable()`
/// and activation work off the disk decision; the row lag heals when the
/// activation update runs.
#[test]
fn crash_between_decision_write_and_row_update_heals_at_activate() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let gen_x = generation_of(&service, &install(&service, "req-p16").task_id);
    let installed = service
        .generations()
        .verify_installed(&gen_x, Utc::now())
        .unwrap();
    // Crash residue: a once-written VERIFIED_COMPATIBLE decision with no
    // row update behind it.
    let artifact = x_data_mcp::generation::DecisionArtifact {
        schema: x_data_mcp::generation::DecisionArtifact::SCHEMA.into(),
        generation_id: gen_x.clone(),
        manifest_sha256: installed.manifest_sha256.clone(),
        issuer: "x-data-acquire.auto-probe".into(),
        validator_contract_version: format!(
            "{}+{}",
            x_data_mcp::file_consistency::CONTRACT_VERSION,
            x_data_mcp::acquire::service::PROBE_CONTRACT_VERSION
        ),
        evaluated_at: Utc::now(),
        status: x_data_mcp::file_consistency::ConsistencyStatus::VerifiedCompatible,
        reasons: vec![],
        evidence_sha256: None,
        probes: serde_json::json!({}),
    };
    let decision_bytes = serde_json::to_vec_pretty(&artifact).unwrap();
    let decision_sha = service
        .generations()
        .write_decision(&gen_x, &decision_bytes)
        .unwrap();
    drop(service);

    let service = open(root.path(), mirror.path());
    // Row lags the artifact (documented divergence): usable() reads the
    // disk decision, so byte- and consistency-wise the generation is
    // already servable.
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_x).as_deref(),
        Some("byte_verified")
    );
    assert!(service.generations().usable(&gen_x, Utc::now()).is_ok());
    // A replayed verify returns the recorded decision; the reply still
    // surfaces the row state as stored.
    let reply = service.verify_generation("req-p16-v", &gen_x).unwrap();
    assert_eq!(
        reply.consistency_status.as_deref(),
        Some("VERIFIED_COMPATIBLE")
    );
    assert_eq!(
        reply.decision_sha256.as_deref(),
        Some(decision_sha.as_str())
    );
    assert_eq!(reply.state, "byte_verified");
    // Activation heals the row: pointer swap + row update, then the row
    // is the single active.
    assert_eq!(activate(&service, &gen_x, None).state, "active");
    let status = service.sync_status().unwrap();
    assert_eq!(generation_state(&status, &gen_x).as_deref(), Some("active"));
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_x);
}

/// P17: crash between `retire_bytes` and `upsert(evidence_only)`
/// (`service.rs:1045-1046`). Bytes are gone but the row still carries
/// its old state. The retry is mechanically idempotent: missing files
/// are already-absent deletions, then the row flips to evidence_only.
#[test]
fn crash_between_retire_bytes_and_row_update_retries_idempotent() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let gate = gate_fixture(root.path());
    let service = open_gated(root.path(), mirror.path(), &gate);
    let gen_a = generation_of(&service, &install(&service, "req-p17a").task_id);
    verify(&service, &gen_a);
    // The gate's evidence must survive the simulated crash: it lives in the
    // source store and the market index, not in the deleting generation.
    satisfy_gate(&service, &gate, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    remirror(mirror.path(), 600007, today_u32());
    let gen_b = generation_of(&service, &install(&service, "req-p17b").task_id);
    verify(&service, &gen_b);
    assert_eq!(
        service
            .activate_generation("activate-p17b", &gen_b, Some(&gen_a))
            .unwrap()
            .state,
        "active"
    );
    let manifest_sha_a = service
        .sync_status()
        .unwrap()
        .generations
        .iter()
        .find(|g| g.generation_id == gen_a)
        .unwrap()
        .manifest_sha256
        .clone();

    // Crash residue: the bytes `retire_bytes` deleted, the row it never
    // updated (still superseded).
    service.generations().retire_bytes(&gen_a).unwrap();
    drop(service);

    let service = open_gated(root.path(), mirror.path(), &gate);
    // Mechanically determinable mid-state: byte checks fail closed,
    // audit artifacts remain, the row is unchanged.
    assert!(
        service
            .generations()
            .verify_installed(&gen_a, Utc::now())
            .is_err()
    );
    assert!(service.generations().decision(&gen_a).unwrap().is_some());
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_a).as_deref(),
        Some("superseded")
    );
    // Retrying the same retire completes the recorded transition.
    let retired = service
        .retire_generation("req-p17-retire", &gen_a, &manifest_sha_a)
        .unwrap();
    assert_eq!(retired.state, "evidence_only");
    assert_eq!(
        generation_state(&service.sync_status().unwrap(), &gen_a).as_deref(),
        Some("evidence_only")
    );
    // A second retire is the stable terminal state, not an error.
    let again = service
        .retire_generation("req-p17-retire-2", &gen_a, &manifest_sha_a)
        .unwrap();
    assert_eq!(again.state, "evidence_only");
    // The active generation kept its bytes throughout.
    assert!(service.generations().usable(&gen_b, Utc::now()).is_ok());
}

// ---------- adoption boundaries ----------

/// P18: a generation directory the DB never recorded (crash between the
/// install rename and every later write, DB rebuilt elsewhere). Reconcile
/// adopts it as `byte_verified`; a copied-in decision artifact replays
/// normally and the generation can verify+activate.
#[test]
fn crash_orphan_generation_dir_is_adopted_byte_verified() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let source = tempfile::tempdir().unwrap();
    let src_service = open(source.path(), mirror.path());
    let gen_x = generation_of(&src_service, &install(&src_service, "req-p18").task_id);
    verify(&src_service, &gen_x);
    let expected = dir_digest(&generation_dir(source.path(), &gen_x));
    drop(src_service);

    // The crash-instant state: generation dir exists, acquire.sqlite has
    // never heard of it.
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("generations")).unwrap();
    copy_dir(
        &generation_dir(source.path(), &gen_x),
        &generation_dir(root.path(), &gen_x),
    );
    let service = open(root.path(), mirror.path());
    let status = service.sync_status().unwrap();
    assert_eq!(
        generation_state(&status, &gen_x).as_deref(),
        Some("byte_verified"),
        "orphan dir must be adopted, not deleted or ignored"
    );
    assert_eq!(status.generations.len(), 1);
    // Bytes adopt unchanged.
    assert_eq!(dir_digest(&generation_dir(root.path(), &gen_x)), expected);
    // The carried-over decision replays; activation works end-to-end.
    let reply = service.verify_generation("req-p18-v", &gen_x).unwrap();
    assert_eq!(
        reply.consistency_status.as_deref(),
        Some("VERIFIED_COMPATIBLE")
    );
    assert_eq!(activate(&service, &gen_x, None).state, "active");
}

/// P19: the on-disk pointer references a generation the DB only knows
/// through adoption (row state `byte_verified`, outside the realign set
/// `{active,superseded,consistency_verified}`). Deterministic outcome
/// today: the row/pointer divergence persists (row stays byte_verified
/// while `active` reports the generation), and retire stays fail-closed
/// through the pointer check. Recorded as a divergence observation in
/// RESULT-PAR-2.
#[test]
fn crash_pointer_to_unrecorded_generation_diverges_row_but_stays_fail_closed() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let source = tempfile::tempdir().unwrap();
    let src_service = open(source.path(), mirror.path());
    let gen_x = generation_of(&src_service, &install(&src_service, "req-p19").task_id);
    let installed = src_service
        .generations()
        .verify_installed(&gen_x, Utc::now())
        .unwrap();
    let manifest_sha = installed.manifest_sha256.clone();
    drop(src_service);

    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("generations")).unwrap();
    copy_dir(
        &generation_dir(source.path(), &gen_x),
        &generation_dir(root.path(), &gen_x),
    );
    // Pointer on disk, generations row absent.
    let pointer = x_data_mcp::generation::ActivePointer {
        schema: x_data_mcp::generation::ActivePointer::SCHEMA.into(),
        generation_id: gen_x.clone(),
        manifest_sha256: manifest_sha.clone(),
        activated_at: Utc::now(),
        previous_generation_id: None,
        activation_request_id: "external".into(),
    };
    std::fs::write(
        root.path().join("active-generation.json"),
        serde_json::to_vec_pretty(&pointer).unwrap(),
    )
    .unwrap();

    let service = open(root.path(), mirror.path());
    let status = service.sync_status().unwrap();
    // The pointer is the read-path source of truth for `active`…
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_x);
    // …but the adopted row was inserted as byte_verified and the
    // pointer-realign loop does not cover that state: the divergence is
    // durable and deterministic.
    assert_eq!(
        generation_state(&status, &gen_x).as_deref(),
        Some("byte_verified")
    );
    // Retire stays fail-closed via the pointer check even though the row
    // does not say `active`.
    let refused = service
        .retire_generation("req-p19-retire", &gen_x, &manifest_sha)
        .unwrap();
    assert_eq!(refused.state, "refused");
    assert_eq!(refused.reason_code.as_deref(), Some("generation_active"));
    drop(service);
    // And nothing changes on reopen.
    let service = open(root.path(), mirror.path());
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().generation_id, gen_x);
    assert_eq!(
        generation_state(&status, &gen_x).as_deref(),
        Some("byte_verified")
    );
}
