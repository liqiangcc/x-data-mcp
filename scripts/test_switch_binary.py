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
import contextlib
import hashlib
import json
import os
import re
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
import data_unit_layout as layout  # noqa: E402
import systemd_exec_start as exec_start  # noqa: E402

SYSTEMCTL_STUB = """#!/usr/bin/env bash
# Test stand-in for systemctl: log argv, honour one injected failure, never act.
# `cat` and `show -p ExecStart` are reads for ExecStart resolution, not actions,
# so they are not logged. XDM_TEST_DATA_UNIT is the base unit body,
# XDM_TEST_DROPIN_UNIT the drop-in (`systemctl cat` concatenates both) and
# XDM_TEST_EFFECTIVE_EXEC what `systemctl show -p ExecStart --value` reports;
# leaving the last one unset exercises the `cat` fallback.
if [[ "$1" == cat ]]; then
    if [[ -n "${XDM_TEST_DATA_UNIT:-}" && "$2" == *-data.service ]]; then
        cat "$XDM_TEST_DATA_UNIT"
        if [[ -n "${XDM_TEST_DROPIN_UNIT:-}" ]]; then
            printf '# %s\\n' "$XDM_TEST_DROPIN_UNIT"
            cat "$XDM_TEST_DROPIN_UNIT"
        fi
    fi
    exit 0
fi
if [[ "$1" == show && "$*" == *ExecStart* ]]; then
    if [[ -n "${XDM_TEST_EFFECTIVE_EXEC:-}" && "$2" == *-data.service ]]; then
        printf '{ path=%s ; argv[]=%s ; ignore_errors=no ; start_time=[n/a] ; }\\n' \\
            "${XDM_TEST_EFFECTIVE_EXEC%% *}" "$XDM_TEST_EFFECTIVE_EXEC"
    fi
    exit 0
fi
printf '%s\\n' "$*" >>"${XDM_TEST_SYSTEMCTL_LOG:-/dev/null}"
if [[ -n "${XDM_TEST_SYSTEMCTL_FAIL:-}" && "$*" == *"$XDM_TEST_SYSTEMCTL_FAIL"* ]]; then
    exit 1
fi
unit=${!#}
state_file="${XDM_TEST_SYSTEMCTL_STATE:-}/${unit}"
case "$1" in
    show) printf 'ControlGroup=/stub\\n' ;;
    is-active)
        state=active
        [[ -n "${XDM_TEST_SYSTEMCTL_STATE:-}" && -f "$state_file" ]] && state=$(cat "$state_file")
        [[ "$state" == active ]] || exit 3
        [[ "$*" == *--quiet* ]] || echo active
        ;;
    stop|start)
        if [[ -n "${XDM_TEST_SYSTEMCTL_STATE:-}" ]]; then
            mkdir -p "$XDM_TEST_SYSTEMCTL_STATE"
            echo "$([[ "$1" == stop ]] && echo inactive || echo active)" >"$state_file"
        fi
        ;;
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
                 acquire_tools=("acquire_generation",), unit_exec=None,
                 entry_tools=None, dropin_exec=None, effective_exec=True):
        self.tmp = tmp
        self.root = deploy_root(tmp)
        self.assets = release_fixture(tmp, SHA)
        self.stub_bin = tmp / "stub-bin"
        self.stub_bin.mkdir()
        write_stub(self.stub_bin / "systemctl", SYSTEMCTL_STUB)
        write_stub(self.stub_bin / "gh", GH_STUB)
        self.log = tmp / "systemctl.log"
        self.state = tmp / "systemctl-state"
        self.data_tools = data_tools
        self.acquire_tools = acquire_tools
        self.unit_exec = unit_exec
        self.entry_tools = entry_tools or {}
        self.unit_file = None
        self.dropin_file = None
        self.effective_exec = None
        self.effective_enabled = effective_exec
        if unit_exec is not None:
            self.set_unit(unit_exec, dropin_exec)

    def set_unit(self, unit_exec, dropin_exec=None):
        """Install a base unit and (optionally) the production drop-in.

        The drop-in clears the inherited `ExecStart=` before setting its own,
        exactly like `exec-start.conf`; what systemd would report through
        `show` is the drop-in's line, while `cat` keeps both snippets.
        """
        self.unit_exec = unit_exec
        self.unit_file = self.tmp / "data.service"
        self.unit_file.write_text(f"[Service]\nExecStart={unit_exec}\n")
        self.dropin_file = None
        self.effective_exec = None
        self.served_exec = unit_exec
        if dropin_exec is not None:
            self.dropin_file = self.tmp / "exec-start.conf"
            self.dropin_file.write_text(
                f"[Service]\nExecStart=\nExecStart={dropin_exec}\n"
            )
            self.effective_exec = dropin_exec
            self.served_exec = dropin_exec

    def endpoints(self):
        if self.unit_exec is None:
            return [("ops", self.root / "run" / "mcp.sock")]
        parsed = layout.parse(self.served_exec)["endpoints"]
        return [(e["entry"], Path(e["socket"])) for e in parsed]

    def tools_for(self, entry):
        if entry == "ops":
            return self.data_tools
        return self.entry_tools.get(entry, ())

    def env(self, **extra):
        env = {
            "PATH": f"{self.stub_bin}{os.pathsep}{os.environ['PATH']}",
            "XDM_TEST_RELEASE_DIR": str(self.assets),
            "XDM_TEST_SYSTEMCTL_LOG": str(self.log),
            "XDM_TEST_SYSTEMCTL_STATE": str(self.state),
        }
        if self.unit_file is not None:
            env["XDM_TEST_DATA_UNIT"] = str(self.unit_file)
        if self.dropin_file is not None:
            env["XDM_TEST_DROPIN_UNIT"] = str(self.dropin_file)
        if self.effective_exec is not None and self.effective_enabled:
            env["XDM_TEST_EFFECTIVE_EXEC"] = self.effective_exec
        env.update(extra)
        return env

    def calls(self):
        return self.log.read_text().splitlines() if self.log.exists() else []

    def run(self, *args, **extra_env):
        with contextlib.ExitStack() as stack:
            for entry, path in self.endpoints():
                stack.enter_context(
                    FakeService(path, mcp_handler(tools=self.tools_for(entry)))
                )
            stack.enter_context(
                FakeService(self.root / "run" / "acquire.sock",
                            mcp_handler(tools=self.acquire_tools))
            )
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
                # XDM-160: the tunnel is stopped first (it Wants/After data
                # and Restart=always), then acquire, then data.
                "DRY: systemctl stop x-data-mcp-tunnel.service",
                "DRY: systemctl stop x-data-mcp-acquire.service",
                "DRY: systemctl stop x-data-mcp-data.service",
                "DRY: ln -sfn",
                "DRY: mv -T",
                "DRY: systemctl start",
                "wait_service_ready.py",
                "--endpoint ops=",
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
            self.assertEqual(set(probe["data_entries"]), {"ops"})
            self.assertEqual(probe["data_entries"]["ops"]["tool"], "plan_market_update")
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
            self.assertIn("start x-data-mcp-data.service", calls)
            self.assertIn("stop x-data-mcp-acquire.service", calls)

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
            # Waiting budget must be >=120s (the old 100/200 x 0.1s is gone).
            self.assertIn("XDM_READY_TIMEOUT:-120", text, path)
            self.assertEqual(subprocess.run(["bash", "-n", str(path)]).returncode, 0, path)
        # deploy_phase_g generates the three-socket unit probes (checklist §3).
        deploy = DEPLOY.read_text()
        for tool in ("plan_market_update", "query_data", "compute_metrics"):
            self.assertIn(f"--expect-tool {tool}", deploy)
        self.assertIn('probe_args+=(--endpoint "$spec")', deploy)
        # switch_binary probes every parsed socket with that entry's tool.
        switch = SCRIPT.read_text()
        self.assertIn('data_args+=(--endpoint "$spec")', switch)
        self.assertIn("--expect-tool acquire_generation", switch)
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
            '|| fail "data socket(s) did not become ready"',
            '--expect-tool acquire_generation || fail "acquire socket did not become ready"',
        ):
            self.assertIn(needle, deploy)
        self.assertIn(
            '--expect-tool plan_market_update || fail "service socket did not become ready"',
            PREVIEW.read_text(),
        )


class ExpectedSchemaFromRelease(unittest.TestCase):
    """#383: the fresh-init schema is read from the release commit.

    preview.36 init'd schema 24 while the script still required 21, so a real
    deploy failed at init and rolled back. The number now travels with the
    code: it is parsed from `src/sqlite_tasks.rs` at the tag's SHA before
    anything is mutated.
    """

    def test_no_hardcoded_fresh_schema(self):
        text = DEPLOY.read_text()
        for needle in (
            "task schema 21 ready",
            '== "21"',
            "schema=20",
            "user_version=20",
            "step=schema21_fresh",
        ):
            self.assertNotIn(needle, text, needle)
        self.assertIn("step=schema_fresh", text)

    def test_expected_schema_is_read_before_any_mutation(self):
        text = DEPLOY.read_text()
        step_at = text.index("step=expected_schema")
        self.assertLess(step_at, text.index("MUTATING=1"))
        # Only top-level stops matter; the rollback helper's stops are indented.
        first_stop = re.search(r"^systemctl stop", text, re.MULTILINE)
        self.assertIsNotNone(first_stop)
        self.assertLess(step_at, first_stop.start())
        # The number comes from the release commit's source tree, and the tag
        # SHA is already resolved by then (the step must not depend on HEAD).
        self.assertIn('git -C "$REPO" show "$SHA:src/sqlite_tasks.rs"', text)
        self.assertIn('fail "cannot read COMPACT_SCHEMA_VERSION at $SHA"', text)
        self.assertIn("XDM_EXPECTED_SCHEMA:-", text)

    def test_both_init_checks_compare_against_expected_schema(self):
        text = DEPLOY.read_text()
        self.assertIn('*"task schema $EXPECTED_SCHEMA ready"*', text)
        self.assertIn('[[ "$fresh_schema" == "$EXPECTED_SCHEMA" ]]', text)
        self.assertIn("user_version=$fresh_schema", text)

    def test_extraction_pattern_matches_current_source(self):
        text = DEPLOY.read_text()
        start = text.index("step=expected_schema")
        block = text[start:text.index("MUTATING=1", start)]
        expr = re.search(r"sed -nE '(s/[^']+)'", block)
        self.assertIsNotNone(expr, "expected_schema step lost its COMPACT_SCHEMA_VERSION sed")
        # s/<pattern>/<replacement>/p -> use the pattern and check it parses the
        # version out of the source that ships it.
        self.assertTrue(expr.group(1).startswith("s/"), expr.group(1))
        pattern = expr.group(1).split("/")[1]
        source = (REPO / "src" / "sqlite_tasks.rs").read_text()
        match = re.search(pattern, source, re.MULTILINE)
        self.assertIsNotNone(
            match, f"{pattern!r} does not match src/sqlite_tasks.rs"
        )
        self.assertGreater(int(match.group(1)), 0)


class TunnelDropinTemplate(unittest.TestCase):
    """#296 P1-3: the tunnel drop-in lives in the repo as a printed template."""

    def test_template_calls_the_repo_probe_from_the_current_symlink(self):
        text = DEPLOY.read_text()
        self.assertIn("tunnel_dropin_template()", text)
        body = text.split("tunnel_dropin_template() {", 1)[1]
        # Ops-D (XDM-166 scheme (a), executed 2026-11-02): the tunnel forwards
        # the read-only research entry, so the probe asserts that entry's tool.
        self.assertIn(
            "ExecStartPre=$DEPLOY_ROOT/current/scripts/wait_service_ready.py "
            "$DEPLOY_ROOT/run/research.sock --timeout 120 --entry research "
            "--expect-tool query_data",
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
            # The generated body is the phase-2 three-socket shape and keeps
            # the production --poll-ms tail.
            exec_lines = [
                line.strip() for line in proc.stderr.splitlines()
                if line.strip().startswith("ExecStart=")
            ]
            self.assertEqual(len(exec_lines), 2, exec_lines)  # data + acquire
            data_line = exec_lines[0]
            for fragment in (
                f"{root}/run/mcp.sock --entry ops",
                f"{root}/run/research.sock --entry research",
                f"{root}/run/maint.sock --entry maint",
                "--poll-ms 1000",
            ):
                self.assertIn(fragment, data_line)
            for fragment in (
                "--expect-tool query_data",
                "--expect-tool compute_metrics",
            ):
                self.assertIn(fragment, proc.stderr)
            # A dry run that stops at the preconditions wrote nothing.
            self.assertFalse(root.exists())



def multi_socket_exec(root: Path, tail="--poll-ms 1000") -> str:
    """The phase-2 unit shape: one db, three sockets, production poll tail."""
    return (
        f"{root}/current/bin/data_service {root}/state/preview.sqlite "
        f"{root}/run/mcp.sock --entry ops "
        f"{root}/run/research.sock --entry research "
        f"{root}/run/maint.sock --entry maint {tail}"
    ).strip()


def run_dry_with_unit(tmp: Path, root: Path, unit_exec: str, *args,
                      dropin_exec: str | None = None, effective: bool = True):
    stub_bin = tmp / "stub-bin"
    stub_bin.mkdir(parents=True)
    write_stub(stub_bin / "systemctl", SYSTEMCTL_STUB)
    unit = tmp / "data.service"
    unit.write_text(f"[Service]\nExecStart={unit_exec}\n")
    env = dict(
        os.environ,
        PATH=f"{stub_bin}{os.pathsep}{os.environ['PATH']}",
        XDM_DEPLOY_ROOT=str(root),
        XDM_READY_TIMEOUT="5",
        XDM_TEST_DATA_UNIT=str(unit),
    )
    if dropin_exec is not None:
        dropin = tmp / "exec-start.conf"
        dropin.write_text(f"[Service]\nExecStart=\nExecStart={dropin_exec}\n")
        env["XDM_TEST_DROPIN_UNIT"] = str(dropin)
        if effective:
            # What systemd's merged view reports: the drop-in wins.
            env["XDM_TEST_EFFECTIVE_EXEC"] = dropin_exec
    return subprocess.run(
        ["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=180
    )


class MultiSocketUnitShape(unittest.TestCase):
    """v8 phase 2 PR-3: parse the installed shape, keep the drop-in tail.

    Production's data unit carries `--poll-ms 1000` from poll-interval.conf.
    Switching the binary must not silently drop it from the printed target
    body, and readiness must cover every socket the unit binds.
    """

    def test_dry_run_parses_every_socket_and_keeps_the_tail(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            root = deploy_root(tmp)
            proc = run_dry_with_unit(tmp, root, multi_socket_exec(root), TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertIn(
                f"data endpoints -> ops={root}/run/mcp.sock "
                f"research={root}/run/research.sock "
                f"maint={root}/run/maint.sock",
                proc.stdout,
            )
            self.assertIn("data tail args -> --poll-ms 1000", proc.stdout)
            target = [
                line for line in proc.stdout.splitlines()
                if line.strip().startswith("data target ->")
            ]
            self.assertEqual(len(target), 1, proc.stdout)
            # The rendered target keeps the same sockets and the exact tail.
            self.assertIn("--poll-ms 1000", target[0])
            for fragment in (
                "mcp.sock --entry ops",
                "research.sock --entry research",
                "maint.sock --entry maint",
            ):
                self.assertIn(fragment, target[0])

    def test_single_socket_unit_with_tail_still_gets_the_three_socket_target(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            root = deploy_root(tmp)
            # The old shape plus the production drop-in tail: the parser must
            # keep the tail while the printed target adds research/maint.
            unit_exec = (
                f"{root}/current/bin/data_service {root}/state/preview.sqlite "
                f"{root}/run/mcp.sock --poll-ms 1000"
            )
            proc = run_dry_with_unit(tmp, root, unit_exec, TAG, SHA)
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertIn("data endpoints -> ops=", proc.stdout)
            self.assertIn("data tail args -> --poll-ms 1000", proc.stdout)
            target = [
                line for line in proc.stdout.splitlines()
                if line.strip().startswith("data target ->")
            ][0]
            for fragment in (
                "mcp.sock --entry ops",
                "research.sock --entry research",
                "maint.sock --entry maint",
                "--poll-ms 1000",
            ):
                self.assertIn(fragment, target)

    def test_execute_probes_all_three_sockets_and_reports_each_entry(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            harness = ExecuteHarness(
                tmp,
                entry_tools={
                    "research": ("query_data", "get_data_availability"),
                    "maint": ("compute_metrics", "cancel_task"),
                },
            )
            harness.set_unit(multi_socket_exec(harness.root))
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            rep = harness.read_report()
            self.assertEqual(rep["status"], "PASS")
            entries = rep["readiness_probe"]["data_entries"]
            self.assertEqual(set(entries), {"ops", "research", "maint"})
            self.assertEqual(entries["ops"]["tool"], "plan_market_update")
            self.assertEqual(entries["research"]["tool"], "query_data")
            self.assertEqual(entries["research"]["tools"], 2)
            self.assertEqual(entries["maint"]["tool"], "compute_metrics")
            # `data` stays the orchestration socket for older readers.
            self.assertEqual(entries["ops"]["socket"], str(harness.root / "run" / "mcp.sock"))
            s6 = [s for s in rep["steps"] if s["step"] == "S6"][0]
            self.assertIn("3 socket(s)", s6["evidence"])

    def test_execute_fails_when_one_entry_surface_is_wrong(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            # research.sock answers with the ops tool only: the probe must fail
            # the whole S6 and roll the symlink back, not accept "some socket".
            harness = ExecuteHarness(
                tmp,
                entry_tools={"research": ("plan_market_update",),
                             "maint": ("compute_metrics",)},
            )
            harness.set_unit(multi_socket_exec(harness.root))
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("FAILED_AT_S6", proc.stderr)
            rep = harness.read_report()
            self.assertEqual(rep["readiness_probe"]["data"], None)
            self.assertEqual(rep["readiness_probe"]["data_entries"], {})
            self.assertEqual(rep["rollback_outcome"], "restored_previous_release")
            self.assertIn("query_data", proc.stderr)
            self.assertEqual(
                (harness.root / "current").readlink(),
                harness.root / "releases" / PREV,
            )



def single_socket_exec(root: Path, tail="") -> str:
    """The base unit shape before the v8 phase-2 drop-in."""
    return (
        f"{root}/current/bin/data_service {root}/state/preview.sqlite "
        f"{root}/run/mcp.sock {tail}"
    ).strip()


class SystemdExecStartParser(unittest.TestCase):
    """XDM-160: the effective ExecStart, not the base unit's first line."""

    def test_show_value_takes_argv_only(self):
        value = (
            "{ path=/x/bin/data_service ; argv[]=/x/bin/data_service /x/db.sqlite "
            "/x/mcp.sock --entry ops ; ignore_errors=no ; start_time=[n/a] ; }"
        )
        self.assertEqual(
            exec_start.from_show_value(value),
            "/x/bin/data_service /x/db.sqlite /x/mcp.sock --entry ops",
        )

    def test_show_value_requotes_escaped_spaces(self):
        self.assertEqual(
            exec_start.from_show_value("{ argv[]=/x/a\\x20b /x/mcp.sock ; }"),
            "'/x/a b' /x/mcp.sock",
        )

    def test_cat_last_non_empty_execstart_wins_after_a_reset(self):
        text = (
            "# /etc/systemd/system/x-data-mcp-data.service\n"
            "[Service]\n"
            "ExecStart=/x/base /x/db.sqlite /x/mcp.sock\n"
            "# /etc/systemd/system/x-data-mcp-data.service.d/exec-start.conf\n"
            "[Service]\n"
            "ExecStart=\n"
            "ExecStart=/x/three --entry ops /x/research.sock --entry research\n"
        )
        self.assertEqual(
            exec_start.from_cat(text),
            "/x/three --entry ops /x/research.sock --entry research",
        )

    def test_cat_reset_without_a_new_value_has_no_execstart(self):
        with self.assertRaises(exec_start.ExecStartError):
            exec_start.from_cat("[Service]\nExecStart=/x/base\nExecStart=\n")

    def test_cat_joins_continuation_lines(self):
        text = "[Service]\nExecStart=/x/db \\\n    /x/mcp.sock --entry ops\n"
        self.assertEqual(
            exec_start.from_cat(text), "/x/db /x/mcp.sock --entry ops"
        )


class EffectiveExecStartResolution(unittest.TestCase):
    """XDM-160: production keeps the three sockets in a drop-in.

    The base unit still carries the old single-socket ExecStart; the
    `exec-start.conf` drop-in resets it and installs the three-socket line.
    Parsing the base unit's first `ExecStart=` saw only `ops` (preview.41).
    """

    def test_dry_run_reports_every_endpoint_of_the_merged_view(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            root = deploy_root(tmp)
            proc = run_dry_with_unit(
                tmp, root, single_socket_exec(root), TAG, SHA,
                dropin_exec=multi_socket_exec(root),
            )
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertIn(
                f"data endpoints -> ops={root}/run/mcp.sock "
                f"research={root}/run/research.sock "
                f"maint={root}/run/maint.sock",
                proc.stdout,
            )
            self.assertIn("data tail args -> --poll-ms 1000", proc.stdout)
            probe = [l for l in proc.stdout.splitlines()
                     if l.startswith("DRY: python3") and "wait_service_ready.py" in l
                     and "--endpoint" in l]
            self.assertEqual(len(probe), 1, proc.stdout)
            for fragment in (
                f"--endpoint ops={root}/run/mcp.sock",
                f"--endpoint research={root}/run/research.sock",
                f"--endpoint maint={root}/run/maint.sock",
            ):
                self.assertIn(fragment, probe[0])

    def test_cat_fallback_applies_the_dropin_override_rule(self):
        # No `systemctl show` value (older systemd or a stubbed host): the
        # fallback must replay the override rule, not return the base line.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            root = deploy_root(tmp)
            proc = run_dry_with_unit(
                tmp, root, single_socket_exec(root), TAG, SHA,
                dropin_exec=multi_socket_exec(root), effective=False,
            )
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            for fragment in (
                f"ops={root}/run/mcp.sock",
                f"research={root}/run/research.sock",
                f"maint={root}/run/maint.sock",
            ):
                self.assertIn(fragment, proc.stdout)

    def test_execute_probes_all_three_dropin_sockets(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            harness = ExecuteHarness(
                tmp,
                entry_tools={
                    "research": ("query_data",),
                    "maint": ("compute_metrics",),
                },
            )
            harness.set_unit(
                single_socket_exec(harness.root), multi_socket_exec(harness.root)
            )
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            rep = harness.read_report()
            entries = rep["readiness_probe"]["data_entries"]
            self.assertEqual(set(entries), {"ops", "research", "maint"})
            self.assertEqual(entries["research"]["tool"], "query_data")
            self.assertEqual(entries["maint"]["tool"], "compute_metrics")
            s6 = [s for s in rep["steps"] if s["step"] == "S6"][0]
            self.assertIn("3 socket(s)", s6["evidence"])


class MaintenanceWindowOrder(unittest.TestCase):
    """XDM-160: the tunnel must leave first and return last.

    `x-data-mcp-tunnel` has `Wants=`/`After=x-data-mcp-data.service` and
    `Restart=always` (`RestartSec=10`), so a window that stops only
    data/acquire is undone when the tunnel restarts and starts data again.
    """

    STOP = (
        "stop x-data-mcp-tunnel.service",
        "stop x-data-mcp-acquire.service",
        "stop x-data-mcp-data.service",
    )
    START = (
        "start x-data-mcp-data.service",
        "start x-data-mcp-acquire.service",
        "start x-data-mcp-tunnel.service",
    )

    def test_execute_stops_tunnel_first_and_starts_it_last(self):
        with tempfile.TemporaryDirectory() as td:
            harness = ExecuteHarness(Path(td))
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            calls = harness.calls()
            for want in self.STOP + self.START:
                self.assertIn(want, calls)
            self.assertEqual(
                [calls.index(want) for want in self.STOP],
                sorted(calls.index(want) for want in self.STOP),
                calls,
            )
            self.assertEqual(
                [calls.index(want) for want in self.START],
                sorted(calls.index(want) for want in self.START),
                calls,
            )
            rep = harness.read_report()
            s4 = [s for s in rep["steps"] if s["step"] == "S4"][0]
            self.assertIn("all three confirmed inactive", s4["evidence"])
            s6 = [s for s in rep["steps"] if s["step"] == "S6"][0]
            self.assertIn("data,acquire,tunnel", s6["evidence"])

    def test_rollback_restores_data_acquire_tunnel_in_that_order(self):
        with tempfile.TemporaryDirectory() as td:
            harness = ExecuteHarness(Path(td), data_tools=())
            proc = harness.run(TAG, SHA, "--execute")
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertEqual(
                harness.read_report()["rollback_outcome"],
                "restored_previous_release",
            )
            calls = harness.calls()

            def last(cmd):
                return max(i for i, c in enumerate(calls) if c == cmd)

            # The rollback's own stop/start sequence is the last use of each.
            positions = [last(want) for want in self.STOP + self.START]
            self.assertEqual(positions, sorted(positions), calls)



if __name__ == "__main__":
    unittest.main()
