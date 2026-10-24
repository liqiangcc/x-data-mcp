"""CI-runnable checks for scripts/switch_binary.sh (TASK-XP-026 S4).

Covers `bash -n` syntax and dry-run behaviour on a simulated deploy root:
a tempdir with releases/<prev> and a `current` symlink. Dry-run must print
the plan, mutate nothing, and never reach /opt or any real service.
"""
import os
import subprocess
import sys
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


def deploy_root(tmp: Path) -> Path:
    root = tmp / "preview"
    (root / "releases" / PREV / "bin").mkdir(parents=True)
    (root / "current").symlink_to(root / "releases" / PREV)
    return root


def run_script(root: Path, *args: str) -> subprocess.CompletedProcess:
    env = dict(os.environ, XDM_DEPLOY_ROOT=str(root))
    return subprocess.run(
        ["bash", str(SCRIPT), *args], env=env, capture_output=True, text=True
    )


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

    def test_missing_previous_release_dir_fails(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td) / "preview"
            (root / "releases").mkdir(parents=True)
            (root / "current").symlink_to(root / "releases" / "gone")
            proc = run_script(root, TAG, SHA)
            self.assertEqual(proc.returncode, 1)
            self.assertIn("previous release dir missing", proc.stderr)


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
        # switch_binary keeps `trap 'rollback; ...' ERR`; both deploy scripts
        # keep `fail` on the probe line; none may swallow the exit status.
        switch = SCRIPT.read_text()
        self.assertIn("trap 'rollback; rm -rf \"$WORK\"' ERR", switch)
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


if __name__ == "__main__":
    unittest.main()
