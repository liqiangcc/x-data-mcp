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
    activate, generation_of, install, open, remirror, sha, today_u32, verify, write_mirror,
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
    assert_eq!(again.active.unwrap().generation_id, gen_b);
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
