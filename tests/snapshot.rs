use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tempfile::TempDir;
use x_data_mcp::snapshot::inspect;
fn now() -> DateTime<Utc> {
    "2026-09-12T00:00:00Z".parse().unwrap()
}
fn fixture() -> (TempDir, Value) {
    let dir = tempfile::tempdir().unwrap();
    let files=[
        ("base_industry","base.zip", "http://www.tdx.com.cn/products/data/data/dbf/base.zip", b"base".as_slice()),
        ("day","hsjday.zip", "https://data.tdx.com.cn/vipdoc/hsjday.zip", b"day".as_slice()),
        ("gbbq","gbbq.zip", "http://www.tdx.com.cn/products/data/data/dbf/gbbq.zip", b"gbbq".as_slice()),
    ].into_iter().map(|(role,name,url,bytes)| {
        std::fs::write(dir.path().join(name),bytes).unwrap();
        json!({"role":role,"relative_path":name,"source_url":url,"acquisition_started_at":"2026-09-11T01:00:00Z","acquisition_completed_at":"2026-09-11T02:00:00Z","last_modified":null,"etag":null,"bytes":bytes.len(),"sha256":x_data_mcp::day_file::sha256(bytes)})
    }).collect::<Vec<_>>();
    (
        dir,
        json!({"schema":"x-data-mcp.official-file-input.v1","files":files}),
    )
}
#[test]
fn matching_bytes_never_approve_origin_semantics_or_cross_file_consistency() {
    let (dir, mut m) = fixture();
    m["files"][1]["acquisition_completed_at"] = json!("2026-09-11T03:00:00Z");
    let bytes = serde_json::to_vec(&m).unwrap();
    let before = std::fs::read(dir.path().join("hsjday.zip")).unwrap();
    let result = inspect(&bytes, dir.path(), now()).unwrap();
    assert_eq!(result.byte_integrity, "FILE_BYTES_VERIFIED");
    assert_eq!(result.manifest_sha256, x_data_mcp::day_file::sha256(&bytes));
    assert_eq!(result.file_set_consistency, "NOT_VERIFIED");
    assert_eq!(result.declared_origin, "NOT_AUTHENTICATED");
    assert_eq!(result.access_use, "NOT_VERIFIED");
    assert_eq!(result.business_semantics, "NOT_VERIFIED");
    assert_eq!(result.archive_member_binding, "NOT_VERIFIED");
    assert!(!result.production_ready);
    assert_eq!(
        std::fs::read(dir.path().join("hsjday.zip")).unwrap(),
        before
    );
    m["production_ready"] = json!(true);
    assert!(inspect(&serde_json::to_vec(&m).unwrap(), dir.path(), now()).is_err());
}
#[test]
fn invalid_manifest_roles_intervals_hashes_and_paths_reject() {
    let (dir, original) = fixture();
    let mut cases = Vec::new();
    let mut m = original.clone();
    m["files"].as_array_mut().unwrap().pop();
    cases.push(m);
    let mut m = original.clone();
    m["files"][1]["role"] = json!("gbbq");
    cases.push(m);
    for (field, value) in [
        ("relative_path", json!("../outside")),
        ("relative_path", json!("/tmp/outside")),
        ("relative_path", json!("")),
        ("source_url", json!("https://tdx.com.cn.evil/file")),
        ("source_url", json!("https://evil@data.tdx.com.cn/file")),
        ("sha256", json!("BAD")),
        ("bytes", json!(0)),
        ("bytes", json!(99)),
        ("acquisition_started_at", json!("2026-09-11T04:00:00Z")),
        ("acquisition_completed_at", json!("2027-01-01T00:00:00Z")),
    ] {
        let mut m = original.clone();
        m["files"][0][field] = value;
        cases.push(m);
    }
    for m in cases {
        assert!(
            inspect(&serde_json::to_vec(&m).unwrap(), dir.path(), now()).is_err(),
            "{m}"
        );
    }
}

#[test]
fn role_must_match_the_declared_official_download_path() {
    let (dir, original) = fixture();
    for (index, wrong_url) in [
        (0, "https://data.tdx.com.cn/vipdoc/hsjday.zip"),
        (1, "http://www.tdx.com.cn/products/data/data/dbf/base.zip"),
        (2, "http://www.tdx.com.cn/products/data/data/dbf/base.zip"),
    ] {
        let mut manifest = original.clone();
        manifest["files"][index]["source_url"] = json!(wrong_url);
        assert!(inspect(&serde_json::to_vec(&manifest).unwrap(), dir.path(), now()).is_err());
    }
}

#[test]
fn role_must_match_the_local_archive_filename() {
    let (dir, original) = fixture();
    for (index, wrong_name) in [(0, "gbbq.zip"), (1, "base.zip"), (2, "hsjday.zip")] {
        let mut manifest = original.clone();
        manifest["files"][index]["relative_path"] = json!(wrong_name);
        assert!(inspect(&serde_json::to_vec(&manifest).unwrap(), dir.path(), now()).is_err());
    }
    let mut nested = original;
    nested["files"][0]["relative_path"] = json!("private/archive/base.zip");
    std::fs::create_dir_all(dir.path().join("private/archive")).unwrap();
    std::fs::copy(
        dir.path().join("base.zip"),
        dir.path().join("private/archive/base.zip"),
    )
    .unwrap();
    assert!(inspect(&serde_json::to_vec(&nested).unwrap(), dir.path(), now()).is_ok());
}
#[test]
fn altered_bytes_missing_file_and_external_symlink_reject() {
    let (dir, m) = fixture();
    let bytes = serde_json::to_vec(&m).unwrap();
    std::fs::write(dir.path().join("hsjday.zip"), b"DAY").unwrap();
    assert!(inspect(&bytes, dir.path(), now()).is_err());
    let (dir, mut m) = fixture();
    m["files"][1]["relative_path"] = json!("missing.zip");
    assert!(inspect(&serde_json::to_vec(&m).unwrap(), dir.path(), now()).is_err());
    #[cfg(unix)]
    {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("day"), b"day").unwrap();
        std::os::unix::fs::symlink(outside.path().join("day"), dir.path().join("linked.zip"))
            .unwrap();
        m["files"][1]["relative_path"] = json!("linked.zip");
        assert!(inspect(&serde_json::to_vec(&m).unwrap(), dir.path(), now()).is_err());
    }
}
#[test]
fn real_cli_outputs_only_byte_evidence_and_rejects_without_success_json() {
    let (dir, m) = fixture();
    let path = dir.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_inspect_snapshot"))
        .arg(&path)
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["production_ready"], false);
    assert_eq!(value["byte_integrity"], "FILE_BYTES_VERIFIED");
    std::fs::write(dir.path().join("gbbq.zip"), b"bad").unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_inspect_snapshot"))
        .arg(&path)
        .arg(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
