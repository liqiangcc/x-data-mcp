use super::*;
use crate::sqlite_tasks::SqliteTasks;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
};
fn database() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("runtime.sqlite");
    SqliteTasks::new(&path).initialize().unwrap();
    (dir, path)
}
#[test]
fn guard_busy_while_held_and_reusable_only_after_scope_ends() {
    let (_dir, path) = database();
    let guard = create(&path, "check", "run").unwrap();
    let descriptor = guard.descriptor().clone();
    assert!(matches!(try_acquire(&path, &descriptor), Err(Error::Busy)));
    assert!(matches!(create(&path, "check", "run"), Err(Error::Busy)));
    drop(guard);
    let acquired = try_acquire(&path, &descriptor).unwrap();
    acquired.verify_binding(&path).unwrap();
    assert!(matches!(create(&path, "check", "run"), Err(Error::Busy)));
    drop(acquired);
    assert_eq!(
        create(&path, "check", "run").unwrap().descriptor(),
        &descriptor
    );
}
#[test]
fn guard_rejects_other_database_boot_run_content_and_replaced_file() {
    let (_dir, path) = database();
    let (_other, other_path) = database();
    let guard = create(&path, "check", "run").unwrap();
    let descriptor = guard.descriptor().clone();
    drop(guard);
    assert!(try_acquire(&other_path, &descriptor).is_err());
    assert!(create(&path, "check", "different-run").is_err());
    let mut wrong_boot = descriptor.clone();
    wrong_boot.boot_id = "00000000-0000-0000-0000-000000000000".into();
    assert!(try_acquire(&path, &wrong_boot).is_err());
    let raw = std::fs::read(&descriptor.lock_path).unwrap();
    std::fs::write(&descriptor.lock_path, b"{}").unwrap();
    assert!(try_acquire(&path, &descriptor).is_err());
    std::fs::write(&descriptor.lock_path, &raw).unwrap();
    let held = try_acquire(&path, &descriptor).unwrap();
    // Keep the old inode allocated; identical copied bytes must not substitute it.
    std::fs::rename(
        &descriptor.lock_path,
        descriptor.lock_path.with_extension("old"),
    )
    .unwrap();
    std::fs::write(&descriptor.lock_path, &raw).unwrap();
    assert!(held.verify_binding(&path).is_err());
    assert!(try_acquire(&path, &descriptor).is_err());
}
#[test]
fn inspector_does_not_create_a_missing_guard_or_repair_bad_bytes() {
    let (_dir, path) = database();
    let guard = create(&path, "check", "run").unwrap();
    let descriptor = guard.descriptor().clone();
    drop(guard);
    std::fs::remove_file(&descriptor.lock_path).unwrap();
    assert!(try_acquire(&path, &descriptor).is_err());
    assert!(!descriptor.lock_path.exists());
}
#[test]
fn guard_child() {
    let Ok(path) = std::env::var("X_DATA_MCP_TEST_RUNTIME_DB") else {
        return;
    };
    let guard = create(Path::new(&path), "process-check", "process-run").unwrap();
    println!(
        "GUARD_READY {}",
        serde_json::to_string(guard.descriptor()).unwrap()
    );
    std::io::stdout().flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    if line.trim() == "exit" {
        std::process::exit(106);
    }
}
#[test]
fn actual_process_guard_blocks_live_peer_and_releases_after_return_or_exit() {
    for exit in [false, true] {
        let (_dir, path) = database();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "verification_runtime::tests::guard_child",
                "--nocapture",
            ])
            .env("X_DATA_MCP_TEST_RUNTIME_DB", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let descriptor: GuardDescriptor = loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if let Some(value) = line.strip_prefix("GUARD_READY ") {
                break serde_json::from_str(value.trim()).unwrap();
            }
        };
        assert!(matches!(try_acquire(&path, &descriptor), Err(Error::Busy)));
        writeln!(
            child.stdin.as_mut().unwrap(),
            "{}",
            if exit { "exit" } else { "return" }
        )
        .unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(if exit { 106 } else { 0 }));
        let after = try_acquire(&path, &descriptor).unwrap();
        after.verify_binding(&path).unwrap();
    }
}
