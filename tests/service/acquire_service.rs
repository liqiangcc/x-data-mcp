//! Phase B (#260) integration coverage: durable acquisition, D1 probe-
//! gated verification, D5 conditional activation/retire, D4 pinned source
//! sessions, crash reconciliation and the MCP adapter contract.
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use x_data_mcp::acquire::fetch::{self, Fetcher};
use x_data_mcp::acquire::wire;
use x_data_mcp::acquire::{AcquireApi, AcquireService, Source};
use x_data_mcp::mcp::ToolHandler;

fn sha(bytes: &[u8]) -> String {
    x_data_mcp::hash::sha256(bytes)
}

fn today_u32() -> u32 {
    x_data_mcp::calendar::shanghai_date(chrono::Utc::now())
        .format("%Y%m%d")
        .to_string()
        .parse()
        .unwrap()
}

fn day_record(date: u32) -> [u8; 32] {
    let mut r = [0u8; 32];
    r[0..4].copy_from_slice(&date.to_le_bytes());
    r[24..28].copy_from_slice(&100u32.to_le_bytes());
    r
}
fn day_member(dates: &[u32]) -> Vec<u8> {
    dates.iter().flat_map(|d| day_record(*d)).collect()
}
fn gbbq_record(market: u8, code: &str, date_raw: u32, category: u8) -> [u8; 29] {
    let mut clear = [0u8; 29];
    clear[0] = market;
    clear[1..1 + code.len()].copy_from_slice(code.as_bytes());
    clear[8..12].copy_from_slice(&date_raw.to_le_bytes());
    clear[12] = category;
    x_data_mcp::gbbq::encrypt_record(&clear)
}
fn gbbq_member(records: &[[u8; 29]]) -> Vec<u8> {
    let mut bytes = (records.len() as u32).to_le_bytes().to_vec();
    for r in records {
        bytes.extend_from_slice(r);
    }
    bytes
}
fn write_zip(path: &Path, members: &[(&str, &[u8])]) {
    let file = std::fs::File::create(path).unwrap();
    let mut w = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        w.start_file(*name, options).unwrap();
        w.write_all(bytes).unwrap();
    }
    w.finish().unwrap();
}
/// A mirror directory the `Mirror` fetcher serves: base/gbbq/hsjday zips
/// whose members satisfy the whole D1 probe suite for scoped members
/// sh.600006 and sz.000001.
fn write_mirror(dir: &Path, last_bar: u32) {
    std::fs::create_dir_all(dir).unwrap();
    let tdxhy = b"1|600006|T090302|||X6102\r\n0|000001|T1001|||X500102\r\n";
    let ex = b"600006|name-one|kw\r\n000001|name-two|kw\r\n".as_slice();
    write_zip(
        &dir.join("base.zip"),
        &[("tdxhy.cfg", tdxhy.as_slice()), ("infoharbor_ex.code", ex)],
    );
    write_zip(
        &dir.join("gbbq.zip"),
        &[(
            "gbbq",
            gbbq_member(&[gbbq_record(1, "600006", 20240101, 1)]).as_slice(),
        )],
    );
    let bars = day_member(&[last_bar]);
    write_zip(
        &dir.join("hsjday.zip"),
        &[
            ("sh\\lday\\sh600006.day", bars.as_slice()),
            ("sz\\lday\\sz000001.day", day_member(&[last_bar]).as_slice()),
        ],
    );
}
/// Point the mirror at a different scoped member (fresh generation bytes).
fn remirror(dir: &Path, code: u32, last_bar: u32) {
    let tdxhy = format!("1|{code}|T090302|||X6102\r\n").into_bytes();
    let ex = format!("{code}|name|kw\r\n").into_bytes();
    write_zip(
        &dir.join("base.zip"),
        &[
            ("tdxhy.cfg", tdxhy.as_slice()),
            ("infoharbor_ex.code", ex.as_slice()),
        ],
    );
    let name = format!("sh\\lday\\sh{code}.day");
    write_zip(
        &dir.join("hsjday.zip"),
        &[(name.as_str(), day_member(&[last_bar]).as_slice())],
    );
}
fn open(root: &Path, mirror: &Path) -> AcquireService {
    AcquireService::open(root, Source::Mirror(mirror.into())).unwrap()
}
fn drive(service: &AcquireService) {
    for _ in 0..16 {
        if service.worker_once().unwrap().is_none() {
            break;
        }
    }
}
fn install(service: &AcquireService, request: &str) -> wire::AcquisitionAcceptance {
    let acceptance = service.acquire_generation(request).unwrap();
    drive(service);
    acceptance
}
fn generation_of(service: &AcquireService, task_id: &str) -> String {
    service.task(task_id).unwrap().generation_id.unwrap()
}
fn verify(service: &AcquireService, generation_id: &str) -> wire::VerifyReply {
    service
        .verify_generation(&format!("verify-{:.16}", generation_id), generation_id)
        .unwrap()
}
fn activate(
    service: &AcquireService,
    generation_id: &str,
    expected: Option<&str>,
) -> wire::ActivateReply {
    service
        .activate_generation(
            &format!("activate-{:.16}", generation_id),
            generation_id,
            expected,
        )
        .unwrap()
}

#[test]
fn acquire_verify_activate_full_lifecycle() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    // Read-only probes report facts and create no work.
    let first = service.check_updates().unwrap();
    assert_eq!(first.files.len(), 3);
    assert!(service.sync_status().unwrap().generations.is_empty());
    let second = service.check_updates().unwrap();
    assert!(
        second
            .files
            .iter()
            .all(|f| f.change == wire::Change::Unchanged),
        "mirror ETags must compare unchanged: {second:?}"
    );
    assert_eq!(second.pending_roles.len(), 3);
    assert!(second.active_generation_id.is_none());

    let acceptance = install(&service, "req-lifecycle");
    let task = service.task(&acceptance.task_id).unwrap();
    assert_eq!(task.state, "succeeded");
    let generation_id = task.generation_id.unwrap();
    assert_eq!(generation_id.len(), 64);

    let reply = verify(&service, &generation_id);
    assert_eq!(reply.state, "consistency_verified", "{reply:?}");
    assert_eq!(
        reply.consistency_status.as_deref(),
        Some("VERIFIED_COMPATIBLE")
    );
    assert!(reply.decision_sha256.is_some());

    let activated = activate(&service, &generation_id, None);
    assert_eq!(activated.state, "active", "{activated:?}");
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.as_ref().unwrap().generation_id, generation_id);
    assert_eq!(status.generations.len(), 1);
    assert_eq!(status.generations[0].state, "active");
    let active_probe = service.check_updates().unwrap();
    assert_eq!(
        active_probe.active_generation_id.as_deref(),
        Some(generation_id.as_str())
    );
    assert!(active_probe.pending_roles.is_empty());
    assert!(
        active_probe
            .files
            .iter()
            .all(|f| f.active_change == wire::Change::Unchanged)
    );
}

#[test]
fn repeated_probe_keeps_remote_diff_pending_until_activation() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let generation_id = generation_of(&service, &install(&service, "req-active-pending").task_id);
    verify(&service, &generation_id);
    activate(&service, &generation_id, None);
    let baseline = service.check_updates().unwrap();
    assert!(baseline.pending_roles.is_empty());

    write_zip(
        &mirror.path().join("hsjday.zip"),
        &[
            (
                "sh\\lday\\sh600006.day",
                day_member(&[today_u32(), today_u32() + 1]).as_slice(),
            ),
            (
                "sz\\lday\\sz000001.day",
                day_member(&[today_u32(), today_u32() + 1]).as_slice(),
            ),
        ],
    );

    let first = service.check_updates().unwrap();
    assert_eq!(first.changed, vec!["day"]);
    assert_eq!(first.pending_roles, vec!["day"]);

    let second = service.check_updates().unwrap();
    assert!(second.changed.is_empty());
    assert_eq!(second.pending_roles, vec!["day"]);
    let day = second.files.iter().find(|file| file.role == "day").unwrap();
    assert_eq!(day.change, wire::Change::Unchanged);
    assert_eq!(day.active_change, wire::Change::Changed);
    assert!(day.active.is_some());
}

#[test]
fn request_replay_and_crash_reconciliation() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    let first = service.acquire_generation("req-replay").unwrap();
    let replay = service.acquire_generation("req-replay").unwrap();
    assert_eq!(first.task_id, replay.task_id);
    assert!(replay.reused);
    // Same request id with a different kind/payload fails closed.
    assert!(service.verify_generation("req-replay", "x").is_err());

    // Drive two stages, drop the service (simulated crash), reopen: the
    // task reconciles to accepted and resumes at its committed stage under
    // the same identity.
    service.worker_once().unwrap();
    service.worker_once().unwrap();
    drop(service);
    let service = open(root.path(), mirror.path());
    let task = service.task(&first.task_id).unwrap();
    assert!(matches!(task.state.as_str(), "accepted" | "running"));
    drive(&service);
    let task = service.task(&first.task_id).unwrap();
    assert_eq!(task.state, "succeeded");
    assert_eq!(task.generation_id.unwrap().len(), 64);
    let request = service.request("req-replay").unwrap();
    assert!(request.bound);
    assert!(request.terminal_result.is_some());
}

#[test]
fn verify_decision_written_once_and_corrupt_bytes_fail_closed() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    let acceptance = install(&service, "req-verdict");
    let generation_id = generation_of(&service, &acceptance.task_id);
    let first = verify(&service, &generation_id);
    assert_eq!(first.state, "consistency_verified");
    // A second verify replays the once-written decision verbatim.
    let second = verify(&service, &generation_id);
    assert!(second.reused);
    assert_eq!(second.decision_sha256, first.decision_sha256);

    // Corrupt an installed artifact: byte re-verification and activation
    // both fail closed; nothing repairs or substitutes.
    let dir = root.path().join("generations").join(&generation_id);
    let mut bytes = std::fs::read(dir.join("gbbq.zip")).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(dir.join("gbbq.zip"), bytes).unwrap();
    let refused = activate(&service, &generation_id, None);
    assert_eq!(refused.state, "refused");
    assert_eq!(
        refused.reason_code.as_deref(),
        Some("generation_bytes_unverified")
    );
}

#[test]
fn stale_generation_mix_stays_not_verified_and_never_activates() {
    let stale = tempfile::tempdir().unwrap();
    write_mirror(stale.path(), 20200103);
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), stale.path());
    let acceptance = install(&service, "req-stale");
    let generation_id = generation_of(&service, &acceptance.task_id);
    let reply = verify(&service, &generation_id);
    assert_eq!(reply.state, "not_verified", "{reply:?}");
    let activated = activate(&service, &generation_id, None);
    assert_eq!(activated.state, "refused");
    assert_eq!(
        activated.reason_code.as_deref(),
        Some("generation_not_consistency_verified")
    );
}

#[test]
fn malformed_archive_never_gains_evidence() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    // Malformed .day member inside a byte-verified archive: truncated to
    // break the 32-byte record framing.
    let mut bad = day_member(&[today_u32()]);
    bad.truncate(10);
    write_zip(
        &mirror.path().join("hsjday.zip"),
        &[("sh\\lday\\sh600006.day", bad.as_slice())],
    );
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let acceptance = install(&service, "req-malformed");
    let generation_id = generation_of(&service, &acceptance.task_id);
    let reply = verify(&service, &generation_id);
    assert_eq!(reply.state, "not_verified", "{reply:?}");
    assert_eq!(activate(&service, &generation_id, None).state, "refused");
}

#[test]
fn conditional_activation_conflict_and_replay() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());

    let a = install(&service, "req-a");
    let gen_a = generation_of(&service, &a.task_id);
    verify(&service, &gen_a);
    assert_eq!(activate(&service, &gen_a, None).state, "active");

    // A second generation (different bytes -> different identity).
    remirror(mirror.path(), 600007, today_u32());
    let b = install(&service, "req-b");
    let gen_b = generation_of(&service, &b.task_id);
    assert_ne!(gen_a, gen_b);
    verify(&service, &gen_b);

    // Stale expectation refuses durably; the same request replays it.
    let refused = activate(&service, &gen_b, None);
    assert_eq!(refused.state, "refused");
    assert_eq!(refused.reason_code.as_deref(), Some("activation_conflict"));
    let replay = activate(&service, &gen_b, None);
    assert!(replay.reused);
    assert_eq!(replay.state, "refused");
    // Same request id with a different payload is a conflict, never a
    // silent second meaning.
    assert!(
        service
            .activate_generation(&format!("activate-{:.16}", gen_b), &gen_b, Some(&gen_a))
            .is_err()
    );

    // Correct expectation under a fresh request id activates.
    let activated = service
        .activate_generation("activate-b-final", &gen_b, Some(&gen_a))
        .unwrap();
    assert_eq!(activated.state, "active");
    assert_eq!(
        activated.previous_generation_id.as_deref(),
        Some(gen_a.as_str())
    );
    let status = service.sync_status().unwrap();
    assert_eq!(status.active.unwrap().generation_id, gen_b);
    assert!(
        status
            .generations
            .iter()
            .any(|g| g.generation_id == gen_a && g.state == "superseded")
    );
}

#[test]
fn pins_block_retire_and_retire_keeps_evidence_only() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    let a = install(&service, "req-pin-a");
    let gen_a = generation_of(&service, &a.task_id);
    verify(&service, &gen_a);
    let manifest_sha = service.sync_status().unwrap().generations[0]
        .manifest_sha256
        .clone();

    // Pinned, not yet active: wrong token refuses before the pin check.
    let pin = service.generations().pin(&gen_a, "task:held").unwrap();
    let retire = service
        .retire_generation("req-retire-1", &gen_a, &sha(b"wrong"))
        .unwrap();
    assert_eq!(retire.state, "refused");
    assert_eq!(
        retire.reason_code.as_deref(),
        Some("confirm_token_mismatch")
    );
    // Correct token, live pin: the referenced generation is protected.
    let retire = service
        .retire_generation("req-retire-2", &gen_a, &manifest_sha)
        .unwrap();
    assert_eq!(retire.state, "refused");
    assert_eq!(retire.reason_code.as_deref(), Some("generation_referenced"));
    drop(pin);

    // Active generations are never retireable.
    assert_eq!(activate(&service, &gen_a, None).state, "active");
    let retire = service
        .retire_generation("req-retire-3", &gen_a, &manifest_sha)
        .unwrap();
    assert_eq!(retire.state, "refused");
    assert_eq!(retire.reason_code.as_deref(), Some("generation_active"));

    // Superseded + unpinned + confirmed: bytes retire to evidence_only.
    remirror(mirror.path(), 600007, today_u32());
    let b = install(&service, "req-pin-b");
    let gen_b = generation_of(&service, &b.task_id);
    verify(&service, &gen_b);
    let pin = service.generations().pin(&gen_a, "task:held-2").unwrap();
    assert_eq!(
        service
            .activate_generation("activate-pin-b", &gen_b, Some(&gen_a))
            .unwrap()
            .state,
        "active"
    );
    // The pinned session's generation stays usable after the pointer swap.
    assert!(
        service
            .generations()
            .usable(&gen_a, chrono::Utc::now())
            .is_ok()
    );
    let retire = service
        .retire_generation("req-retire-4", &gen_a, &manifest_sha)
        .unwrap();
    assert_eq!(retire.state, "refused");
    drop(pin);
    let retire = service
        .retire_generation("req-retire-5", &gen_a, &manifest_sha)
        .unwrap();
    assert_eq!(retire.state, "evidence_only");
    // Bytes are gone; byte-level replay fails closed, audit artifacts stay.
    assert!(
        service
            .generations()
            .verify_installed(&gen_a, chrono::Utc::now())
            .is_err()
    );
    assert!(service.generations().decision(&gen_a).unwrap().is_some());
    let activated = service
        .activate_generation("activate-retired", &gen_a, Some(&gen_b))
        .unwrap();
    assert_eq!(activated.state, "refused");
}

#[test]
fn hot_capacity_refuses_new_candidates_without_deletion() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let service = open(root.path(), mirror.path());
    for i in 0..6u32 {
        remirror(mirror.path(), 600010 + i, today_u32());
        install(&service, &format!("req-cap-{i}"));
    }
    assert_eq!(service.sync_status().unwrap().generations.len(), 6);
    assert!(service.acquire_generation("req-cap-overflow").is_err());
}

#[test]
fn head_change_states_and_no_etag_stays_unknown() {
    struct Fake {
        head: Mutex<fetch::Head>,
    }
    impl Fetcher for Fake {
        fn head(&self, _url: &str) -> Result<fetch::Head, fetch::Error> {
            Ok(self.head.lock().unwrap().clone())
        }
        fn fetch(&self, _url: &str, _dest: &Path) -> Result<fetch::FetchResult, fetch::Error> {
            unreachable!("check-only fixture never fetches")
        }
    }
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake {
        head: Mutex::new(fetch::Head {
            status: 200,
            etag: Some("\"v1\"".into()),
            last_modified: Some("Wed, 01 Jan 2025 00:00:00 GMT".into()),
            content_length: Some(100),
        }),
    });
    let fetcher: Arc<dyn Fetcher> = fake.clone();
    let service = AcquireService::open(root.path(), Source::Fetcher(fetcher)).unwrap();

    // First observation without a prior fact is never "unchanged".
    let first = service.check_updates().unwrap();
    assert!(
        first
            .files
            .iter()
            .all(|f| f.change == wire::Change::Unknown)
    );

    let second = service.check_updates().unwrap();
    assert!(
        second
            .files
            .iter()
            .all(|f| f.change == wire::Change::Unchanged)
    );

    // ETag change -> changed; no-etag comparison -> unknown even when
    // length/last-modified match (AC-A03).
    fake.head.lock().unwrap().etag = Some("\"v2\"".into());
    let changed = service.check_updates().unwrap();
    assert!(
        changed
            .files
            .iter()
            .all(|f| f.change == wire::Change::Changed)
    );
    assert_eq!(changed.changed.len(), 3);

    fake.head.lock().unwrap().etag = None;
    let no_etag = service.check_updates().unwrap();
    assert!(
        no_etag
            .files
            .iter()
            .all(|f| f.change == wire::Change::Unknown)
    );
    assert_eq!(no_etag.unknown.len(), 3);
}

#[test]
fn mcp_tools_dispatch_and_capability_facts() {
    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    let api = AcquireApi::new(open(root.path(), mirror.path()));

    let tools = api.tools();
    let names: Vec<&str> = tools
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "check_official_updates",
        "acquire_generation",
        "get_acquisition_task",
        "get_acquisition_request",
        "verify_generation",
        "activate_generation",
        "retire_generation",
        "sync_status",
        "run_worker_once",
        "get_acquisition_capabilities",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
    // No workflow leakage: tool surface carries facts only.
    let listed = serde_json::to_string(&tools).unwrap();
    for banned in ["next_actions", "next_tool"] {
        assert!(!listed.contains(banned), "tools/list leaked {banned}");
    }

    let v1 = |extra: Value| {
        let mut o = extra.as_object().unwrap().clone();
        o.insert("contract_version".into(), json!("v1"));
        Value::Object(o)
    };
    let check = api.call("check_official_updates", v1(json!({}))).unwrap();
    assert_eq!(check["files"].as_array().unwrap().len(), 3);
    assert_eq!(check["pending_roles"].as_array().unwrap().len(), 3);
    assert!(
        check["files"].as_array().unwrap()[0]
            .get("active_change")
            .is_some()
    );

    let accepted = api
        .call("acquire_generation", v1(json!({"request_id": "mcp-req"})))
        .unwrap();
    assert_eq!(accepted["reused"], json!(false));
    let replay = api
        .call("acquire_generation", v1(json!({"request_id": "mcp-req"})))
        .unwrap();
    assert_eq!(replay["reused"], json!(true));
    assert_eq!(accepted["task_id"], replay["task_id"]);

    let task = api
        .call(
            "get_acquisition_task",
            v1(json!({"task_id": accepted["task_id"].as_str().unwrap()})),
        )
        .unwrap();
    assert_eq!(task["task_id"], accepted["task_id"]);

    let request = api
        .call(
            "get_acquisition_request",
            v1(json!({"request_id": "mcp-req"})),
        )
        .unwrap();
    assert_eq!(request["bound"], json!(true));

    // Empty input, matching the runtime worker loop's call shape.
    let turn = api.call("run_worker_once", json!({})).unwrap();
    assert_eq!(turn["idle"], json!(false));

    let caps = api
        .call("get_acquisition_capabilities", v1(json!({})))
        .unwrap();
    assert_eq!(
        caps["implementation"]["capability_revision"],
        json!("x-data-acquire-v1")
    );
    assert_eq!(caps["schema_digest"].as_str().unwrap().len(), 64);
    assert_eq!(caps["active_generation_ref"], Value::Null);
    assert!(
        caps["blocking_reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["code"] == "no_active_generation")
    );

    // Unsupported contract, unknown tools and malformed arguments all fail
    // at decode with no side effects.
    let bad = api
        .call(
            "acquire_generation",
            json!({"contract_version":"v2","request_id":"x"}),
        )
        .unwrap_err();
    assert_eq!(bad.code, "unsupported_contract");
    let unknown = api.call("not_a_tool", v1(json!({}))).unwrap_err();
    assert_eq!(unknown.code, "unknown_tool");
    let missing_args = api.call("acquire_generation", v1(json!({}))).unwrap_err();
    assert_eq!(missing_args.code, "invalid_arguments");
}

/// Daemon-level E2E: the real `acquire_service` binary serves the whole
/// acquire → verify → activate loop over its private socket with the
/// runtime worker driving stages — no shell, no direct store access.
#[cfg(target_os = "linux")]
#[test]
fn daemon_serves_full_generation_loop_over_mcp() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let mirror = tempfile::tempdir().unwrap();
    write_mirror(mirror.path(), today_u32());
    let root = tempfile::tempdir().unwrap();
    // The owner-only endpoint requires a private parent directory.
    std::fs::set_permissions(
        root.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .unwrap();
    let socket = root.path().join("acq.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_acquire_service"))
        .arg(root.path())
        .arg(&socket)
        .args(["--poll-ms", "50"])
        .arg("--mirror")
        .arg(mirror.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            child.try_wait().unwrap().is_none(),
            "daemon exited before readiness"
        );
        if x_data_mcp::local_runtime::connect(&socket).is_ok() {
            break;
        }
        assert!(Instant::now() < deadline, "daemon readiness deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
    struct Session {
        stream: std::os::unix::net::UnixStream,
        reader: BufReader<std::os::unix::net::UnixStream>,
        seq: u64,
    }
    impl Session {
        fn request(&mut self, method: &str, params: Value) -> Value {
            self.seq += 1;
            writeln!(
                self.stream,
                "{}",
                json!({"jsonrpc":"2.0","id":self.seq,"method":method,"params":params})
            )
            .unwrap();
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0);
            let v: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(v["id"], self.seq);
            v
        }
        fn call(&mut self, name: &str, args: Value) -> Value {
            let v = self.request("tools/call", json!({"name":name,"arguments":args}));
            assert_eq!(v["result"]["isError"], false, "{name} failed: {v}");
            v["result"]["structuredContent"].clone()
        }
    }
    let stream = x_data_mcp::local_runtime::connect(&socket).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut session = Session {
        reader: BufReader::new(stream.try_clone().unwrap()),
        stream,
        seq: 0,
    };

    assert!(session.request("initialize", json!({}))["result"]["serverInfo"].is_object());
    let tools = session.request("tools/list", json!({}))["result"]["tools"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(tools.len(), 10);

    let accepted = session.call(
        "acquire_generation",
        json!({"contract_version":"v1","request_id":"daemon-e2e"}),
    );
    let task_id = accepted["task_id"].as_str().unwrap().to_string();
    // The runtime worker drives fetch→manifest→install at 50ms ticks.
    let deadline = Instant::now() + Duration::from_secs(30);
    let generation_id = loop {
        let task = session.call(
            "get_acquisition_task",
            json!({"contract_version":"v1","task_id":task_id}),
        );
        if task["state"] == "succeeded" {
            break task["generation_id"].as_str().unwrap().to_string();
        }
        assert_ne!(task["state"], "failed", "{task}");
        assert!(Instant::now() < deadline, "task never finished: {task}");
        std::thread::sleep(Duration::from_millis(100));
    };
    let verified = session.call(
        "verify_generation",
        json!({"contract_version":"v1","request_id":"daemon-verify","generation_id":generation_id}),
    );
    assert_eq!(verified["state"], "consistency_verified", "{verified}");
    let activated = session.call(
        "activate_generation",
        json!({"contract_version":"v1","request_id":"daemon-activate","generation_id":generation_id}),
    );
    assert_eq!(activated["state"], "active", "{activated}");
    let status = session.call("sync_status", json!({"contract_version":"v1"}));
    assert_eq!(
        status["active"]["generation_id"],
        json!(generation_id),
        "{status}"
    );
    let caps = session.call(
        "get_acquisition_capabilities",
        json!({"contract_version":"v1"}),
    );
    assert_eq!(caps["runtime_readiness"], "ready", "{caps}");
    assert_eq!(caps["active_generation_ref"], json!(generation_id));

    // SAFETY: test owns this child PID and only sends SIGTERM to it.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "daemon stop deadline");
        std::thread::sleep(Duration::from_millis(10));
    }
}
