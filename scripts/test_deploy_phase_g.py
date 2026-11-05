"""CI-runnable checks for the directory skeleton of scripts/deploy_phase_g.sh.

A new host has none of `releases/`, `reports/`, the database directory, the
generation root or the socket directory, and the script used to fail the first
run at `install_release` because `releases/` was missing. These tests run the
real script against a tempdir deploy root with stand-ins only:

  * `systemctl` is a shell stub that serves the unit bodies and tracks state;
  * the release comes from a local git "origin" through the script's own
    `--force-git-transport` path (refs/dist/<tag>), no network, no `gh`;
  * the release binaries (task_sqlite, source_store, mcp_connect, ...) are
    small scripts; the sockets are the FakeService helper from
    test_wait_service_ready.py, bound as soon as the script creates run/.

Nothing touches /opt, a real unit or a real service.
"""
import contextlib
import hashlib
import json
import os
import stat
import subprocess
import tarfile
import tempfile
import threading
import time
import unittest
from pathlib import Path

from test_switch_binary import (
    DEPLOY,
    REPO,
    SHA,
    TAG,
    multi_socket_exec,
    write_stub,
)
from test_wait_service_ready import FakeService, mcp_handler

SCHEMA = "27"
ACQ_TOOLS = ("check_official_updates", "acquire_generation", "activate_generation")
OPS_TOOLS = ("update_data", "start_collection", "get_data_availability",
             "query_data", "plan_market_update")
ENTRY_TOOLS = {"research": ("query_data",), "maint": ("compute_metrics",)}

SYSTEMCTL_STUB = """#!/usr/bin/env bash
# Test stand-in: serve unit bodies for `cat`, track start/stop, never act.
case "$1" in
    cat)
        case "$2" in
            *-data.service) cat "$XDM_TEST_DATA_UNIT" ;;
            *-acquire.service) cat "$XDM_TEST_ACQ_UNIT" ;;
            *) exit 1 ;;
        esac
        exit 0 ;;
    show) exit 0 ;;
esac
printf '%s\\n' "$*" >>"$XDM_TEST_SYSTEMCTL_LOG"
unit=${!#}
case "$1" in
    is-active)
        state=active
        [[ -f "$XDM_TEST_SYSTEMCTL_STATE/$unit" ]] && state=$(cat "$XDM_TEST_SYSTEMCTL_STATE/$unit")
        [[ "$state" == active ]] || exit 3 ;;
    stop|start)
        mkdir -p "$XDM_TEST_SYSTEMCTL_STATE"
        echo "$([[ "$1" == stop ]] && echo inactive || echo active)" >"$XDM_TEST_SYSTEMCTL_STATE/$unit" ;;
esac
exit 0
"""

TASK_SQLITE = f"""#!/usr/bin/env python3
import sqlite3, sys
if sys.argv[1] != "init21":
    sys.exit(2)
con = sqlite3.connect(sys.argv[2])
con.execute("PRAGMA user_version={SCHEMA}")
con.commit()
print("task schema {SCHEMA} ready")
"""

SOURCE_STORE = """#!/usr/bin/env bash
echo '{"status": "store_empty"}'
"""

# mcp_connect stand-in: stdio <-> unix socket, which is all the smoke needs.
MCP_CONNECT = """#!/usr/bin/env python3
import socket, sys, threading
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
def pump():
    while True:
        data = s.recv(65536)
        if not data:
            break
        sys.stdout.write(data.decode()); sys.stdout.flush()
threading.Thread(target=pump, daemon=True).start()
for line in sys.stdin:
    s.sendall(line.encode())
"""


def git(cwd, *args):
    subprocess.run(
        ["git", "-C", str(cwd), "-c", "user.name=t", "-c", "user.email=t@example.com",
         *args],
        check=True, capture_output=True,
    )


def build_origin(tmp: Path, tarball_digest_lie: bool = False) -> Path:
    """A git repo whose refs/dist/<tag> carries the dist assets, as in CI."""
    name = f"x-data-mcp-{TAG.lstrip('v')}-linux-x86_64"
    stage = tmp / "pkg" / name
    (stage / "bin").mkdir(parents=True)
    (stage / "bin" / "mcp_data").write_text(
        f"#!/usr/bin/env bash\necho 'x-data-mcp {TAG.lstrip('v')} {SHA}'\n")
    (stage / "bin" / "acquire_service").write_text("#!/usr/bin/env bash\nexit 0\n")
    (stage / "bin" / "task_sqlite").write_text(TASK_SQLITE)
    (stage / "bin" / "source_store").write_text(SOURCE_STORE)
    (stage / "bin" / "mcp_connect").write_text(MCP_CONNECT)
    members = sorted(p for p in (stage / "bin").iterdir())
    for p in members:
        p.chmod(0o755)
    (stage / "SHA256SUMS").write_text("".join(
        f"{hashlib.sha256(p.read_bytes()).hexdigest()}  bin/{p.name}\n" for p in members))
    tarball = tmp / "pkg" / f"{name}.tar.gz"
    with tarfile.open(tarball, "w:gz") as tar:
        tar.add(stage, arcname=name)
    digest = hashlib.sha256(tarball.read_bytes()).hexdigest()

    origin = tmp / "origin"
    origin.mkdir()
    git(origin, "init", "-q")
    (origin / tarball.name).write_bytes(tarball.read_bytes())
    (origin / f"{tarball.name}.sha256").write_text(f"{digest}  {tarball.name}\n")
    (origin / "smoke-report.json").write_text(
        json.dumps({"status": "PASS", "version": f"{TAG.lstrip('v')}-{SHA}"}))
    (origin / "dist-manifest.json").write_text(json.dumps({
        "tag": TAG, "tag_sha": SHA, "version": TAG.lstrip("v"),
        "tarball_sha256": ("0" * 64) if tarball_digest_lie else digest}))
    git(origin, "add", "-A")
    git(origin, "commit", "-q", "-m", "dist")
    git(origin, "update-ref", f"refs/dist/{TAG}", "HEAD")
    return origin


class Host:
    """A tempdir deploy root, with whatever pre-existing state a test asks for."""

    def __init__(self, tmp: Path, **origin_opts):
        self.tmp = tmp
        self.root = tmp / "preview"
        origin = build_origin(tmp, **origin_opts)
        self.repo = tmp / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q")
        git(self.repo, "remote", "add", "origin", f"file://{origin}")
        stub_bin = tmp / "stub-bin"
        stub_bin.mkdir()
        write_stub(stub_bin / "systemctl", SYSTEMCTL_STUB)
        self.data_unit = tmp / "data.service"
        self.data_unit.write_text(f"[Service]\nExecStart={multi_socket_exec(self.root)}\n")
        self.acq_unit = tmp / "acquire.service"
        self.acq_unit.write_text(
            "[Service]\nExecStart="
            f"{self.root}/current/bin/acquire_service {self.root}/generations "
            f"{self.root}/run/acquire.sock --market-db {self.root}/state/preview.sqlite\n")
        self.log = tmp / "systemctl.log"
        self.env = dict(
            os.environ,
            PATH=f"{stub_bin}{os.pathsep}{os.environ['PATH']}",
            XDM_DEPLOY_ROOT=str(self.root),
            XDM_READY_TIMEOUT="10",
            XDM_EXPECTED_SCHEMA=SCHEMA,
            REPO=str(self.repo),
            XDM_TEST_DATA_UNIT=str(self.data_unit),
            XDM_TEST_ACQ_UNIT=str(self.acq_unit),
            XDM_TEST_SYSTEMCTL_LOG=str(self.log),
            XDM_TEST_SYSTEMCTL_STATE=str(tmp / "systemctl-state"),
        )

    def sockets(self):
        run = self.root / "run"
        return [
            (run / "mcp.sock", OPS_TOOLS),
            (run / "research.sock", ENTRY_TOOLS["research"]),
            (run / "maint.sock", ENTRY_TOOLS["maint"]),
            (run / "acquire.sock", ACQ_TOOLS),
        ]

    @contextlib.contextmanager
    def serving(self):
        """Bind the fake services as soon as the script has created run/."""
        stop = threading.Event()
        started = []

        def watch():
            pending = self.sockets()
            while pending and not stop.is_set():
                for item in list(pending):
                    path, tools = item
                    if path.parent.is_dir():
                        svc = FakeService(path, mcp_handler(tools=tools)).__enter__()
                        started.append(svc)
                        pending.remove(item)
                time.sleep(0.02)

        thread = threading.Thread(target=watch, daemon=True)
        thread.start()
        try:
            yield
        finally:
            stop.set()
            thread.join(timeout=5)
            for svc in started:
                svc.__exit__(None, None, None)

    def run(self, *args):
        with self.serving():
            return subprocess.run(
                ["bash", str(DEPLOY), TAG, "--force-git-transport", *args],
                env=self.env, capture_output=True, text=True, timeout=120)

    def mode(self, path: Path) -> int:
        return stat.S_IMODE(path.stat().st_mode)

    def report(self):
        return json.loads(next((self.root / "reports").glob("*-phaseg-deployment.json")).read_text())


class NewHostSkeleton(unittest.TestCase):
    def test_first_run_on_an_empty_host_creates_the_skeleton_and_deploys(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td))
            self.assertFalse(host.root.exists())
            proc = host.run()
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            for rel in ("releases", "reports", "state", "generations", "run"):
                directory = host.root / rel
                self.assertTrue(directory.is_dir(), rel)
                self.assertEqual(host.mode(directory), 0o700, rel)
            self.assertEqual(host.mode(host.root), 0o700)
            self.assertTrue((host.root / "releases" / SHA / "bin" / "task_sqlite").is_file())
            self.assertEqual((host.root / "current").readlink(), host.root / "releases" / SHA)
            self.assertEqual(host.report()["status"], "DEPLOYED")
            self.assertEqual(host.report()["source_store_replay"]["status"], "store_empty")
            # The source store is data from the clone step, never skeleton.
            self.assertFalse((host.root / "state" / "source-store").exists())

    def test_dry_run_on_an_empty_host_lists_the_missing_dirs_and_writes_nothing(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td))
            proc = host.run("--dry-run")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertIn(f"missing dirs:    {host.root}", proc.stdout)
            self.assertIn(f"{host.root}/releases", proc.stdout)
            self.assertFalse(host.root.exists())

    def test_a_failure_before_the_package_is_verified_creates_nothing(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td), tarball_digest_lie=True)
            proc = host.run()
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("FAILED_AT_fetch_package", proc.stderr)
            self.assertFalse(host.root.exists())

    def test_a_path_that_is_not_a_directory_fails_closed_and_reports(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td))
            host.root.mkdir(mode=0o700)
            (host.root / "releases").write_text("not a directory")
            proc = host.run()
            self.assertEqual(proc.returncode, 1, proc.stderr + proc.stdout)
            self.assertIn("FAILED_AT_prepare_layout", proc.stderr)
            self.assertIn("rollback: not_needed", proc.stderr)
            self.assertEqual(host.report()["status"], "FAILED_AT_prepare_layout")
            self.assertEqual((host.root / "releases").read_text(), "not a directory")


class ExistingDeployment(unittest.TestCase):
    def test_existing_directories_keep_their_mode_and_content(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td))
            for rel in ("", "releases", "reports", "state", "generations", "run"):
                (host.root / rel).mkdir(mode=0o755, exist_ok=True)
                (host.root / rel).chmod(0o755)
            marker = host.root / "state" / "keep.txt"
            marker.write_text("untouched")
            proc = host.run("--dry-run")
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertIn("missing dirs:    none", proc.stdout)
            proc = host.run()
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            self.assertNotIn("created ", proc.stdout)
            for rel in ("", "releases", "reports", "state", "generations", "run"):
                self.assertEqual(host.mode(host.root / rel), 0o755, rel or "root")
            self.assertEqual(marker.read_text(), "untouched")
            self.assertEqual(host.report()["status"], "DEPLOYED")

    def test_a_pre_existing_database_is_still_archived_not_deleted(self):
        with tempfile.TemporaryDirectory() as td:
            host = Host(Path(td))
            state = host.root / "state"
            state.mkdir(parents=True)
            (host.root / "run").mkdir()
            import sqlite3
            con = sqlite3.connect(state / "preview.sqlite")
            con.execute("PRAGMA user_version=26")
            con.execute("CREATE TABLE t(x)")
            con.commit()
            con.close()
            proc = host.run()
            self.assertEqual(proc.returncode, 0, proc.stderr + proc.stdout)
            archived = list(state.glob("preview.sqlite.v26-archived-*"))
            self.assertEqual(len(archived), 1, list(state.iterdir()))
            self.assertEqual(host.report()["before"], {"schema": 26})


if __name__ == "__main__":
    unittest.main()
