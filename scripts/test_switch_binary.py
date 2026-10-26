"""CI-runnable checks for scripts/switch_binary.sh (TASK-XP-026 S4, #296 P1-3).

Covers `bash -n` syntax and dry-run behaviour on a simulated deploy root:
a tempdir with releases/<prev> and a `current` symlink. Dry-run must print
the plan, mutate nothing, and never reach /opt or any real service.

The report tests (#296 P1-3) drive the `--execute` path against the same
tempdir root with three stand-ins, so no real switch ever happens:
  * `gh` and `systemctl` are shell stubs on PATH (they only log their argv);
  * the release tarball is a fixture built here (bin/mcp_data, bin/*_service,
    check.py, SHA256SUMS) and the stub `gh release download` copies it;
  * both service sockets are real unix sockets served by the FakeService
    helper from test_wait_service_ready.py, so scripts/wait_service_ready.py
    runs unmodified and its JSON is what lands in the report.
"""
import hashlib
import json
import os
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "switch_binary.sh"
DEPLOY = REPO / "scripts" / "deploy_phase_g.sh"
PREVIEW = REPO / "scripts" / "deploy_preview.sh"
PROBE = REPO / "scripts" / "wait_service_ready.py"
# Every deploy entry point must wait through the probe: switch_binary and
# deploy_phase_g cover the two-MCP topology, deploy_preview the legacy
# single-service topology.
DEPLOY_SCRIPTS = (SCRIPT, DEPLOY, PREVIEW)
TWO_MCP_SCRIPTS = (SCRIPT, DEPLOY)
TAG = "v0.2.0-preview.30"
SHA = "1" * 40
PREV = "0" * 40
TAG_SHORT = "preview30"

sys.path.insert(0, str(REPO / "scripts"))
from test_wait_service_ready import FakeService, mcp_handler  # noqa: E402

SYSTEMCTL_STUB = """#!/usr/bin/env bash
# Test stand-in for systemctl: log argv, honour one injected failure, never act.
printf '%s\\n' "$*" >>"${XDM_TEST_SYSTEMCTL_LOG:-/dev/null}"
if [[ -n "${XDM_TEST_SYSTEMCTL_FAIL:-}" && "$*" == *"$XDM_TEST_SYSTEMCTL_FAIL"* ]]; then
    exit 1
fi
case "$1" in
    show) printf 'ControlGroup=/stub\\n' ;;
    is-active) [[ "$*" == *--quiet* ]] || echo active ;;
esac
exit 0
"""

GH_STUB = """#!/usr/bin/env bash
# Test stand-in for `gh release download <tag> -p ... --dir <dir>`.
if [[ "$1" == "release" && "$2" == "download" ]]; then
    dir=""
    shift 2
    while (($#)); do
        case "$1" in
            --dir) dir=$2; shift 2 ;;
            *) shift ;;
        esac
    done
    cp "${XDM_TEST_RELEASE_DIR:?}"/* "$dir"/ && exit 0
fi
echo "gh stub: unsupported invocation: $*" >&2
exit 1
"""


def deploy_root(tmp: Path) -> Path:
    root = tmp / "preview"
    (root / "releases" / PREV / "bin").mkdir(parents=True)
    (root / "run").mkdir()
    (root / "current").symlink_to(root / "releases" / PREV)
    return root


def run_script(root: Path, *args: str, env_extra=None, timeout=180):
    env = dict(os.environ, XDM_DEPLOY_ROOT=str(root), XDM_READY_TIMEOUT="5")
    env.pop("XDM_TEST_SYSTEMCTL_FAIL", None)
    env.update(env_extra or {})
    return subprocess.run(
        ["bash", str(SCRIPT), *args],
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def write_stub(path: Path, body: str) -> Path:
    path.write_text(body)
    path.chmod(0o755)
    return path


def release_fixture(tmp: Path, sha: str, tag: str = TAG) -> Path:
    """Build the assets `gh release download` would fetch (tarball, .sha256, smoke)."""
    assets = tmp / "assets"
    assets.mkdir()
    name = f"x-data-mcp-{tag.lstrip('v')}-linux-x86_64"
    stage = tmp / "fixture-stage"
    (stage / "bin").mkdir(parents=True)
    (stage / "bin" / "mcp_data").write_text(
        f"#!/usr/bin/env bash\necho 'x-data-mcp {tag.lstrip('v')} ({sha})'\n"
    )
    for binary in ("acquire_service", "data_service"):
        (stage / "bin" / binary).write_text("#!/usr/bin/env bash\nexit 0\n")
    (stage / "check.py").write_text(
        "import json, pathlib, sys\n"
        "out = pathlib.Path(sys.argv[sys.argv.index('--report') + 1])\n"
        "out.write_text(json.dumps({'status': 'PASS'}))\n"
    )
    for path in [*sorted((stage / "bin").iterdir()), stage / "check.py"]:
        path.chmod(0o755)
    members = ["bin/acquire_service", "bin/data_service", "bin/mcp_data", "check.py"]
    (stage / "SHA256SUMS").write_text(
        "".join(
            f"{hashlib.sha256((stage / member).read_bytes()).hexdigest()}  {member}\n"
            for member in members
        )
    )
    tarball = assets / f"{name}.tar.gz"
    with tarfile.open(tarball, "w:gz") as tar:
        tar.add(stage, arcname=name)
    (assets / f"{name}.tar.gz.sha256").write_text(
        f"{hashlib.sha256(tarball.read_bytes()).hexdigest()}  {tarball.name}\n"
    )
    (assets / "smoke-report.json").write_text(
        json.dumps({"status": "PASS", "version": f"{tag.lstrip('v')}-{sha}"})
    )
    return assets


class ExecuteHarness:
    """A tempdir deploy root plus PATH stubs for gh/systemctl and real sockets."""

    def __init__(self, tmp: Path, data_tools=("plan_market_update",),
                 acquire_tools=("acquire_generation",)):
        self.tmp = tmp
        self.root = deploy_root(tmp)
        self.assets = release_fixture(tmp, SHA)
        self.stub_bin = tmp / "stub-bin"
        self.stub_bin.mkdir()
        write_stub(self.stub_bin / "systemctl", SYSTEMCTL_STUB)
        write_stub(self.stub_bin / "gh", GH_STUB)
        self.log = tmp / "systemctl.log"
        self.data_tools = data_tools
        self.acquire_tools = acquire_tools

    def env(self, **extra):
        env = {
            "PATH": f"{self.stub_bin}{os.pathsep}{os.environ['PATH']}",
            "XDM_TEST_RELEASE_DIR": str(self.assets),
            "XDM_TEST_SYSTEMCTL_LOG": str(self.log),
        }
        env.update(extra)
        return env

    def run(self, *args, **extra_env):
        with FakeService(self.root / "run" / "mcp.sock",
                         mcp_handler(tools=self.data_tools)), \
             FakeService(self.root / "run" / "acquire.sock",
                         mcp_handler(tools=self.acquire_tools)):
            return run_script(self.root, *args, env_extra=self.env(**extra_env))

    def reports(self):
        directory = self.root / "reports"
        return sorted(directory.glob("*.json")) if directory.is_dir() else []

    def read_report(self, path=None):
        path = path or self.reports()[0]
        return json.loads(path.read_text())


class SwitchBinary(unittest.TestCase):
    def test_bash_syntax(self):
        self.assertEqual(
            subprocess.run(["bash", "-n", str(SCRIPT)]).returncode, 0
        )

    def test_dry_run_prints_plan_and_mutates_nothing(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            out = proc.stdout
            for marker in (
                "DRY: gh release download",
                "DRY: systemctl stop x-data-mcp-acquire",
                "DRY: systemctl stop x-data-mcp-data",
                "DRY: ln -sfn",
                "DRY: mv -T",
                "DRY: systemctl start",
                "wait_service_ready.py",
                "--expect-tool plan_market_update",
                "--expect-tool acquire_generation",
            ):
                self.assertIn(marker, out)
            self.assertIn("ROLLBACK", out)
            self.assertEqual((root / "current").readlink(), root / "releases" / PREV)
            self.assertFalse((root / "current.new").exists())
            self.assertFalse((root / "current.rb").exists())
            self.assertFalse((root / "releases" / SHA).exists())

    def test_dry_run_is_default(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertNotIn("+ systemctl", proc.stdout)

    def test_bad_args_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            for args in [
                ("not-a-tag", SHA),
                (TAG, "deadbeef"),
                ("v0.2.0-preview.30x", SHA),
            ]:
                proc = run_script(root, *args)
                self.assertEqual(proc.returncode, 2, args)

    def test_unknown_flag_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            proc = run_script(root, TAG, SHA, "--explode")
            self.assertEqual(proc.returncode, 2, proc.stderr)
            self.assertIn("unknown flag", proc.stderr)

    def test_missing_previous_release_dir_fails(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td) / "preview"
            (root / "releases").mkdir(parents=True)
            (root / "current").symlink_to(root / "releases" / "gone")
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 1)
            self.assertIn("previous release dir missing", proc.stderr)
            # A failing dry-run still writes nothing into ROOT.
            self.assertFalse((root / "reports").exists())


class DryRunWritesNothing(unittest.TestCase):
    """#296 P1-3: a dry-run must not create anything under ROOT."""

    def test_dry_run_creates_no_report(self):
        with tempfile.TemporaryDirectory() as td:
            root = deploy_root(Path(td))
            before = sorted(p.relative_to(root) for p in root.rglob("*"))
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertIn("DRY: report would be written to", proc.stdout)
            self.assertIn(f"reports/{TAG_SHORT}-switch-", proc.stdout)
            self.assertFalse((root / "reports").exists())
            self.assertEqual(
                before, sorted(p.relative_to(root) for p in root.rglob("*"))
            )

    def test_dry_run_ignores_report_flag(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            root = deploy_root(tmp)
            explicit = tmp / "explicit-report.json"
            proc = run_script(root, TAG, SHA, "--report", str(explicit))
            self.assertEqual(proc.returncode, 0, proc.stderr)
            self.assertFalse(explicit.exists())
            self.assertFalse((root / "reports").exists())
            self.assertIn(str(explicit), proc.stdout)


class ReportSchema(unittest.TestCase):
    """#296 P1-3: --execute produces the deploy_phase_g-style machine report."""

    def assert_step_records(self, steps, expected_status="PASS"):
        self.assertEqual(
            [s["step"] for s in steps], ["P", "S1", "S2", "S3", "S4", "S5", "S6", "S7"]
        )
        for record in steps:
            self.assertEqual(
                set(record), {"step", "name", "status", "elapsed_ms", "evidence"}
            )
            self.assertEqual(record["status"], expected_status, record)
            self.assertIsInstance(record["elapsed_ms"], int)
            # Sanity bound: a milliseconds field must not carry seconds or
            # nanoseconds (a %3N-unaware coreutils printed ns on 2026-10-26).
            self.assertGreaterEqual(record["elapsed_ms"], 0)
            self.assertLess(record["elapsed_ms"], 600_000, record)
            self.assertTrue(record["evidence"], record)

    def test_execute_writes_full_report(self):
        with tempfile.TemporaryDirectory() as td:
            harness = ExecuteHarness(Path(td))
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            reports = harness.reports()
            self.assertEqual(len(reports), 1, reports)
            self.assertRegex(
                reports[0].name,
                rf"^{TAG_SHORT}-switch-\d{{8}}T\d{{6}}Z\.json$",
            )
            rep = harness.read_report(reports[0])
            self.assertEqual(rep["status"], "PASS")
            self.assertEqual(rep["kind"], "binary-only switch (no DB migration, no rebuild)")
            self.assertEqual(rep["release"], TAG)
            self.assertEqual(rep["code_sha"], SHA)
            self.assertRegex(rep["started_at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
            self.assertRegex(rep["finished_at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
            self.assertLessEqual(rep["started_at"], rep["finished_at"])
            self.assertIsNone(rep["failed_at"])
            self.assertEqual(rep["previous_release"], str(harness.root / "releases" / PREV))
            self.assertEqual(rep["current_release"], str(harness.root / "releases" / SHA))
            self.assertEqual(rep["rollback_outcome"], "not_needed")
            self.assertEqual(rep["production"], "NOT_PRODUCTION_READY")
            self.assert_step_records(rep["steps"])
            probe = rep["readiness_probe"]
            self.assertEqual(probe["data"]["socket"], str(harness.root / "run" / "mcp.sock"))
            self.assertEqual(probe["data"]["tool"], "plan_market_update")
            self.assertIsInstance(probe["data"]["elapsed_ms"], int)
            self.assertEqual(probe["acquire"]["socket"], str(harness.root / "run" / "acquire.sock"))
            self.assertEqual(probe["acquire"]["tool"], "acquire_generation")
            self.assertEqual((harness.root / "current").readlink(),
                             harness.root / "releases" / SHA)

    def test_report_flag_overrides_default_path(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            harness = ExecuteHarness(tmp)
            explicit = tmp / "evidence" / "switch.json"
            proc = harness.run(TAG, SHA, "--execute", "--report", str(explicit))
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertEqual(harness.reports(), [])
            self.assertTrue(explicit.is_file())
            self.assertEqual(harness.read_report(explicit)["status"], "PASS")
            self.assertIn(f"report: {explicit}", proc.stdout)

    def test_report_flag_equals_form(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            harness = ExecuteHarness(tmp)
            explicit = tmp / "switch-eq.json"
            proc = harness.run(TAG, SHA, "--execute", f"--report={explicit}")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertTrue(explicit.is_file())
            self.assertEqual(harness.read_report(explicit)["release"], TAG)

    def test_readiness_failure_writes_fail_report_and_rolls_back(self):
        with tempfile.TemporaryDirectory() as td:
            # The data socket answers, but without the required tool: the probe
            # exits 2 and the ERR trap must roll back and still write the report.
            harness = ExecuteHarness(Path(td), data_tools=())
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("FAILED_AT_S6", proc.stderr)
            rep = harness.read_report()
            self.assertEqual(rep["status"], "FAIL")
            self.assertEqual(rep["failed_at"], "S6")
            self.assertEqual(rep["rollback_outcome"], "restored_previous_release")
            self.assertEqual(rep["previous_release"], str(harness.root / "releases" / PREV))
            self.assertEqual(rep["current_release"], str(harness.root / "releases" / PREV))
            self.assertEqual(rep["production"], "NOT_PRODUCTION_READY")
            statuses = {s["step"]: s["status"] for s in rep["steps"]}
            self.assertEqual(
                statuses,
                {"P": "PASS", "S1": "PASS", "S2": "PASS", "S3": "PASS",
                 "S4": "PASS", "S5": "PASS", "S6": "FAIL"},
            )
            s6 = [s for s in rep["steps"] if s["step"] == "S6"][0]
            self.assertIn("command failed", s6["evidence"])
            self.assertIn("wait_service_ready.py", s6["evidence"])
            self.assertIsNone(rep["readiness_probe"]["data"])
            # The switch was undone: current is back on the previous release.
            self.assertEqual((harness.root / "current").readlink(),
                             harness.root / "releases" / PREV)
            # systemctl saw the rollback stop/start pair, never a real unit.
            calls = harness.log.read_text().splitlines()
            self.assertIn("start x-data-mcp-data", calls)
            self.assertIn("stop x-data-mcp-acquire", calls)

    def test_failure_before_mutation_reports_not_needed_rollback(self):
        with tempfile.TemporaryDirectory() as td:
            harness = ExecuteHarness(Path(td))
            # A release dir that already exists makes S3 fail before any stop.
            (harness.root / "releases" / SHA).mkdir()
            marker = harness.root / "releases" / SHA / "keep.txt"
            marker.write_text("untouched")
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("FAILED_AT_S3", proc.stderr)
            rep = harness.read_report()
            self.assertEqual(rep["status"], "FAIL")
            self.assertEqual(rep["failed_at"], "S3")
            self.assertEqual(rep["rollback_outcome"], "not_needed")
            statuses = {s["step"]: s["status"] for s in rep["steps"]}
            self.assertEqual(statuses, {"P": "PASS", "S1": "PASS", "S2": "PASS", "S3": "FAIL"})
            s3 = [s for s in rep["steps"] if s["step"] == "S3"][0]
            self.assertIn("release dir already exists", s3["evidence"])
            # Nothing was stopped and the pre-existing release dir is untouched.
            self.assertFalse(harness.log.exists())
            self.assertEqual(marker.read_text(), "untouched")
            self.assertEqual((harness.root / "current").readlink(),
                             harness.root / "releases" / PREV)


class ReadinessWiring(unittest.TestCase):
    """TASK-OPS-T1/T1B: every deploy entry point waits via the single probe."""

    def test_probe_exists_and_is_syntactically_valid(self):
        self.assertTrue(PROBE.is_file(), PROBE)
        proc = subprocess.run([sys.executable, "-B", str(PROBE), "--help"],
                              capture_output=True, text=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("--expect-tool", proc.stdout)

    def test_scripts_call_the_probe_with_expect_tool(self):
        for path in DEPLOY_SCRIPTS:
            text = path.read_text()
            self.assertIn("wait_service_ready.py", text, path)
            self.assertIn("--expect-tool plan_market_update", text, path)
            # Waiting budget must be >=120s (the old 100/200 x 0.1s is gone).
            self.assertIn("XDM_READY_TIMEOUT:-120", text, path)
            self.assertEqual(subprocess.run(["bash", "-n", str(path)]).returncode, 0, path)
        # Only the two-MCP topology has an acquisition socket.
        for path in TWO_MCP_SCRIPTS:
            self.assertIn("--expect-tool acquire_generation", path.read_text(), path)
        self.assertNotIn("--expect-tool acquire_generation", PREVIEW.read_text())

    def test_no_fixed_socket_file_polling_remains(self):
        for path in DEPLOY_SCRIPTS:
            text = path.read_text()
            for needle in ("seq 1 100", "seq 1 200", "sleep 0.1"):
                self.assertNotIn(needle, text, path)

    def test_socket_file_existence_is_not_treated_as_ready(self):
        self.assertNotIn('[[ -S "$ROOT/run/mcp.sock"', SCRIPT.read_text())
        self.assertNotIn('[[ -S "$SOCK_DATA" ]] ||', DEPLOY.read_text())
        self.assertNotIn('[[ -S "$SOCK" ]]', PREVIEW.read_text())

    def test_probe_failure_still_reaches_the_existing_rollback(self):
        # switch_binary routes every failing command (the readiness probe
        # included) through the ERR trap -> fail() -> rollback() -> FAIL report;
        # nothing on the probe line may swallow the exit status.
        switch = SCRIPT.read_text()
        self.assertIn("trap on_error ERR", switch)
        self.assertIn("on_error() { fail \"command failed: $BASH_COMMAND\"; }", switch)
        self.assertIn("    rollback\n    outcome=$ROLLBACK_OUTCOME", switch)
        self.assertIn("write_report FAIL \"$outcome\"", switch)
        self.assertNotIn("--expect-tool plan_market_update || true", switch)
        self.assertNotIn("--expect-tool acquire_generation || true", switch)
        deploy = DEPLOY.read_text()
        for needle in (
            '--expect-tool plan_market_update || fail "data socket did not become ready"',
            '--expect-tool acquire_generation || fail "acquire socket did not become ready"',
        ):
            self.assertIn(needle, deploy)
        self.assertIn(
            '--expect-tool plan_market_update || fail "service socket did not become ready"',
            PREVIEW.read_text(),
        )


class TunnelDropinTemplate(unittest.TestCase):
    """#296 P1-3: the tunnel drop-in lives in the repo as a printed template."""

    def test_template_calls_the_repo_probe_from_the_current_symlink(self):
        text = DEPLOY.read_text()
        self.assertIn("tunnel_dropin_template()", text)
        body = text.split("tunnel_dropin_template() {", 1)[1]
        self.assertIn(
            "ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py "
            "$DEPLOY_ROOT/run/mcp.sock --timeout 120 --expect-tool plan_market_update",
            body,
        )
        # systemd's default TimeoutStartSec (90s) covers ExecStartPre, so the
        # drop-in must raise it above the probe's own 120s budget.
        self.assertIn("TimeoutStartSec=180", body)
        # Template only: nothing in the deploy script writes a unit file.
        for forbidden in ("tee ", "install -m", "systemctl enable", "systemctl daemon-reload"):
            self.assertNotIn(forbidden, text)

    def test_missing_units_print_the_template(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            stub = write_stub(tmp / "systemctl", "#!/usr/bin/env bash\nexit 1\n")
            root = tmp / "preview"
            env = dict(
                os.environ,
                PATH=f"{stub.parent}{os.pathsep}{os.environ['PATH']}",
                XDM_DEPLOY_ROOT=str(root),
            )
            proc = subprocess.run(
                ["bash", str(DEPLOY), TAG, "--dry-run"],
                env=env, capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(proc.returncode, 2, proc.stdout + proc.stderr)
            self.assertIn("missing systemd unit(s)", proc.stderr)
            self.assertIn(
                f"ExecStartPre={root}/current/scripts/wait_service_ready.py "
                f"{root}/run/mcp.sock --timeout 120 --expect-tool plan_market_update",
                proc.stderr,
            )
            # A dry run that stops at the preconditions wrote nothing.
            self.assertFalse(root.exists())


if __name__ == "__main__":
    unittest.main()
