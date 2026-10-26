"""CI-runnable checks for scripts/rollback_preview.sh (#296 P1-3).

The script had no test at all: it restores a bound (backup, previous release)
pair and writes reports/rollback-*.json. Every test here runs it against a
tempdir deploy root with a `systemctl` stand-in on PATH, so the mutation path
is exercised end to end without a real service, a real /opt root or a real
rollback. The post-rollback verification always ends FAIL in a tempdir: it
proves the tunnel cgroup runs the restored release's `mcp_connect`, which no
fixture can create. The report is what these tests pin.
"""
import json
import os
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "rollback_preview.sh"
PREV = "0" * 40
SCHEMA = 21

SYSTEMCTL_STUB = """#!/usr/bin/env bash
# Test stand-in for systemctl: log argv, honour one injected failure, never act.
printf '%s\\n' "$*" >>"${XDM_TEST_SYSTEMCTL_LOG:-/dev/null}"
if [[ -n "${XDM_TEST_SYSTEMCTL_FAIL:-}" && "$*" == *"$XDM_TEST_SYSTEMCTL_FAIL"* ]]; then
    exit 1
fi
case "$1" in
    show) printf 'ControlGroup=/stub-cgroup\\n' ;;
    is-active) [[ "$*" == *--quiet* ]] || echo active ;;
esac
exit 0
"""

TASK_SQLITE_STUB = """#!/usr/bin/env bash
# Test stand-in for `task_sqlite upgrade <copy>`: the restored release accepts
# the schema it finds. It must not modify the live database; it is given a copy.
printf 'task schema %s ready\\n' "${XDM_TEST_SCHEMA:-21}"
"""


def write_stub(path: Path, body: str) -> Path:
    path.write_text(body)
    path.chmod(0o755)
    return path


def make_db(path: Path, schema: int = SCHEMA) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    db = sqlite3.connect(path)
    db.execute("CREATE TABLE marker(value TEXT)")
    db.execute("INSERT INTO marker VALUES ('restored')")
    db.commit()
    db.execute(f"PRAGMA user_version = {schema}")
    db.commit()
    db.close()
    return path


class RollbackHarness:
    """Tempdir deploy root: DB + WAL/SHM, backup, previous release, stubs."""

    def __init__(self, tmp: Path, executable_bins: bool = True):
        self.tmp = tmp
        self.root = tmp / "preview"
        self.db = make_db(self.root / "state" / "preview.sqlite")
        for suffix in ("-wal", "-shm"):
            Path(f"{self.db}{suffix}").write_text("journal")
        self.backup = tmp / "backup-preview.30.sqlite"
        shutil.copy(self.db, self.backup)
        self.prev = self.root / "releases" / PREV
        (self.prev / "bin").mkdir(parents=True)
        write_stub(self.prev / "bin" / "mcp_connect", "#!/usr/bin/env bash\nexit 0\n")
        task_sqlite = self.prev / "bin" / "task_sqlite"
        if executable_bins:
            write_stub(task_sqlite, TASK_SQLITE_STUB)
        else:
            task_sqlite.write_text(TASK_SQLITE_STUB)
        self.root.mkdir(exist_ok=True)
        (self.root / "current").symlink_to(self.prev)
        (self.root / "reports").mkdir()
        self.stub_bin = tmp / "stub-bin"
        self.stub_bin.mkdir()
        write_stub(self.stub_bin / "systemctl", SYSTEMCTL_STUB)
        self.log = tmp / "systemctl.log"

    def env(self, **extra):
        env = {
            "PATH": f"{self.stub_bin}{os.pathsep}{os.environ['PATH']}",
            "XDM_DEPLOY_ROOT": str(self.root),
            "XDM_TEST_SYSTEMCTL_LOG": str(self.log),
            "XDM_TEST_SCHEMA": str(SCHEMA),
        }
        env.pop("XDM_TEST_SYSTEMCTL_FAIL", None)
        env.update(extra)
        return env

    def run(self, *args, **extra_env):
        return subprocess.run(
            ["bash", str(SCRIPT), *args],
            env=self.env(**extra_env),
            capture_output=True,
            text=True,
            timeout=120,
        )

    def reports(self):
        return sorted((self.root / "reports").glob("rollback-*.json"))

    def read_report(self):
        return json.loads(self.reports()[0].read_text())

    def deployment_report(self, name="deployment.json", **fields):
        path = self.tmp / name
        body = {"status": "DEPLOYED", "code_sha": "1" * 40, "release": "v0.2.0-preview.30"}
        body.update(fields)
        path.write_text(json.dumps(body))
        return path


class Syntax(unittest.TestCase):
    def test_bash_syntax(self):
        self.assertEqual(subprocess.run(["bash", "-n", str(SCRIPT)]).returncode, 0)


class FailClosedBeforeMutation(unittest.TestCase):
    """Every input error must exit non-zero before any systemctl call."""

    def test_unknown_flag_prints_usage(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run("--dry-run")
            self.assertEqual(proc.returncode, 2, proc.stderr)
            self.assertIn("usage: rollback_preview.sh", proc.stderr)
            self.assertFalse(harness.log.exists())

    def test_no_args_refuses_without_a_bound_pair(self):
        # There is no dry-run flag: the script either has a bound (backup,
        # previous release) pair or refuses before touching anything.
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run()
            self.assertEqual(proc.returncode, 1, proc.stderr)
            self.assertIn("no explicit backup; refusing to guess", proc.stderr)
            self.assertFalse(harness.log.exists())
            self.assertEqual(harness.reports(), [])

    def test_missing_backup_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run(f"--backup={harness.tmp / 'nope.sqlite'}",
                               f"--previous-release={PREV}")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("no explicit backup; refusing to guess", proc.stderr)
            self.assertFalse(harness.log.exists())
            self.assertEqual(harness.reports(), [])

    def test_backup_without_previous_release_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run(f"--backup={harness.backup}")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("no previous release bound to this backup", proc.stderr)
            self.assertFalse(harness.log.exists())

    def test_missing_previous_release_dir_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run(f"--backup={harness.backup}", "--previous-release=deadbeef")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("previous release dir missing", proc.stderr)
            self.assertFalse(harness.log.exists())

    def test_previous_release_without_executables_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td), executable_bins=False)
            proc = harness.run(f"--backup={harness.backup}", f"--previous-release={PREV}")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("lacks executable bin/task_sqlite or bin/mcp_connect", proc.stderr)
            self.assertFalse(harness.log.exists())

    def test_from_report_without_backup_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            report = harness.deployment_report(previous_release=PREV)
            proc = harness.run(f"--from-report={report}")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("no explicit backup; refusing to guess", proc.stderr)
            self.assertFalse(harness.log.exists())

    def test_from_report_without_previous_release_refuses(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            report = harness.deployment_report(backup=str(harness.backup))
            proc = harness.run(f"--from-report={report}")
            self.assertEqual(proc.returncode, 1)
            self.assertIn("no previous release bound to this backup", proc.stderr)
            self.assertFalse(harness.log.exists())


class MutationAndReport(unittest.TestCase):
    """The non-dry-run path on a tempdir root: it mutates only that root."""

    def test_report_fields_and_restored_state(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            # The live release the rollback starts from is not the previous one.
            live = harness.root / "releases" / ("2" * 40)
            live.mkdir()
            (harness.root / "current").unlink()
            (harness.root / "current").symlink_to(live)
            proc = harness.run(f"--backup={harness.backup}", f"--previous-release={PREV}")
            # Post-rollback verification ends FAIL here: a tempdir has no tunnel
            # cgroup running the restored mcp_connect. The report must still be
            # complete and the rollback itself must have happened.
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            reports = harness.reports()
            self.assertEqual(len(reports), 1, reports)
            self.assertEqual(reports[0].name, f"rollback-{harness.backup.stem}.json")
            rep = harness.read_report()
            self.assertEqual(rep["status"], "FAIL")
            self.assertEqual(rep["restored_backup"], str(harness.backup))
            self.assertEqual(rep["previous_release"], str(harness.prev))
            self.assertEqual(rep["schema"], SCHEMA)
            self.assertEqual(rep["current"], str(harness.prev.resolve()))
            self.assertEqual(rep["production"], "NOT_PRODUCTION_READY")
            self.assertEqual(
                set(rep["checks"]),
                {"integrity", "current_points_at_previous_release",
                 "schema_accepted_by_restored_release", "service_active",
                 "tunnel_active", "tunnel_cgroup_readable",
                 "tunnel_proxy_matches_release"},
            )
            self.assertTrue(rep["checks"]["integrity"])
            self.assertTrue(rep["checks"]["current_points_at_previous_release"])
            self.assertTrue(rep["checks"]["schema_accepted_by_restored_release"])
            self.assertTrue(rep["checks"]["service_active"])
            self.assertTrue(rep["checks"]["tunnel_active"])
            # No tunnel cgroup in a tempdir: this is why the run ends FAIL.
            self.assertFalse(rep["checks"]["tunnel_cgroup_readable"])
            self.assertFalse(rep["checks"]["tunnel_proxy_matches_release"])
            self.assertIn(f"task schema {SCHEMA} ready", rep["schema_check_stdout"])
            self.assertEqual(json.loads(proc.stdout.strip().splitlines()[-1])["status"], "FAIL")
            # The restore itself happened: DB bytes are the backup, no WAL/SHM.
            self.assertEqual(harness.db.read_bytes(), harness.backup.read_bytes())
            self.assertFalse(Path(f"{harness.db}-wal").exists())
            self.assertFalse(Path(f"{harness.db}-shm").exists())
            self.assertEqual((harness.root / "current").readlink(), harness.prev)
            calls = harness.log.read_text().splitlines()
            self.assertEqual(
                calls,
                ["stop x-data-mcp-tunnel.service",
                 "stop x-data-mcp-data.service",
                 "start x-data-mcp-data.service",
                 "restart x-data-mcp-tunnel.service",
                 "show x-data-mcp-tunnel.service -p ControlGroup",
                 "is-active x-data-mcp-data.service",
                 "is-active x-data-mcp-tunnel.service"],
            )

    def test_from_report_binds_backup_and_previous_release(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            report = harness.deployment_report(
                backup=str(harness.backup), previous_release=PREV
            )
            proc = harness.run(f"--from-report={report}")
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            rep = harness.read_report()
            self.assertEqual(rep["restored_backup"], str(harness.backup))
            self.assertEqual(rep["previous_release"], str(harness.prev))

    def test_mutation_failure_writes_fail_report_and_stops(self):
        with tempfile.TemporaryDirectory() as td:
            harness = RollbackHarness(Path(td))
            proc = harness.run(
                f"--backup={harness.backup}", f"--previous-release={PREV}",
                XDM_TEST_SYSTEMCTL_FAIL="stop x-data-mcp-tunnel.service",
            )
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("rollback FAILED_AT_stop_tunnel", proc.stderr)
            rep = harness.read_report()
            self.assertEqual(rep["status"], "FAIL")
            self.assertEqual(rep["failed_at"], "stop_tunnel")
            self.assertEqual(rep["error"], "systemctl stop x-data-mcp-tunnel.service")
            self.assertEqual(rep["restored_backup"], str(harness.backup))
            self.assertEqual(rep["previous_release"], str(harness.prev))
            self.assertEqual(rep["production"], "NOT_PRODUCTION_READY")
            # The mutation phase stopped at the first failed step.
            self.assertEqual(harness.log.read_text().splitlines(),
                             ["stop x-data-mcp-tunnel.service"])
            # Nothing was restored yet, so the live DB still carries its journal.
            self.assertTrue(Path(f"{harness.db}-wal").exists())


if __name__ == "__main__":
    unittest.main()
